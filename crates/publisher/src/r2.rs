use anyhow::{anyhow, bail, Context, Result};
use aws_sdk_s3::config::{Builder, Credentials, Region, RequestChecksumCalculation, ResponseChecksumValidation};
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::primitives::{ByteStream, Length};
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_sdk_s3::Client;
use aws_smithy_types::DateTime;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const MULTIPART_THRESHOLD: u64 = 100 * 1024 * 1024;
const PART_SIZE: u64 = 64 * 1024 * 1024;

pub struct StoredObject {
    pub key: String,
    /// Seconds since the Unix epoch.
    pub modified: i64,
    pub size: u64,
}

/// An upload that was started and never completed or aborted. Its parts are billed as storage.
pub struct OpenUpload {
    pub key: String,
    pub upload_id: String,
    /// Seconds since the Unix epoch.
    pub started: i64,
    pub size: u64,
}

#[derive(Clone)]
pub struct R2 {
    client: Client,
    bucket: String,
}

fn not_found<E>(error: &SdkError<E, aws_sdk_s3::config::http::HttpResponse>) -> bool {
    matches!(error, SdkError::ServiceError(e) if e.raw().status().as_u16() == 404)
}

fn describe<E, R>(error: SdkError<E, R>) -> anyhow::Error
where
    SdkError<E, R>: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
{
    let friendly = match error.code() {
        Some("InvalidAccessKeyId" | "SignatureDoesNotMatch" | "AccessDenied" | "Unauthorized") => {
            Some("Akses R2 ditolak. Cek Account ID, Access Key, dan Secret Key di Pengaturan")
        }
        Some("NoSuchBucket") => Some("Bucket tidak ditemukan. Cek nama bucket di Pengaturan"),
        _ => None,
    };
    match friendly {
        Some(text) => anyhow!(text),
        None => anyhow!(error),
    }
}

impl R2 {
    pub fn cloudflare(account_id: &str, access_key: &str, secret_key: &str, bucket: &str) -> Self {
        let endpoint = format!("https://{}.r2.cloudflarestorage.com", account_id.trim());
        Self::with_endpoint(&endpoint, access_key, secret_key, bucket)
    }

    pub fn with_endpoint(endpoint: &str, access_key: &str, secret_key: &str, bucket: &str) -> Self {
        let credentials = Credentials::new(access_key.trim(), secret_key.trim(), None, None, "fm-uploader");
        let config = Builder::new()
            .region(Region::new("auto"))
            .endpoint_url(endpoint)
            .force_path_style(true)
            .credentials_provider(credentials)
            // R2 does not accept the checksum headers the SDK sends by default.
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
            .build();
        Self { client: Client::from_conf(config), bucket: bucket.trim().to_string() }
    }

    pub async fn size_of(&self, key: &str) -> Result<Option<u64>> {
        match self.client.head_object().bucket(&self.bucket).key(key).send().await {
            Ok(head) => Ok(head.content_length().map(|n| n as u64)),
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(describe(e)),
        }
    }

    pub async fn get_text(&self, key: &str) -> Result<Option<String>> {
        match self.client.get_object().bucket(&self.bucket).key(key).send().await {
            Ok(object) => {
                let bytes = object.body.collect().await.context("Gagal membaca dari R2")?.into_bytes();
                Ok(Some(String::from_utf8(bytes.to_vec())?))
            }
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(describe(e)),
        }
    }

    pub async fn put_text(&self, key: &str, text: String) -> Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(text.into_bytes()))
            .content_type("application/json")
            .send()
            .await
            .map_err(describe)?;
        Ok(())
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        self.client.delete_object().bucket(&self.bucket).key(key).send().await.map_err(describe)?;
        Ok(())
    }

    pub async fn list(&self, prefix: &str) -> Result<Vec<StoredObject>> {
        let mut objects = Vec::new();
        let mut token = None;
        loop {
            let page = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix)
                .set_continuation_token(token)
                .send()
                .await
                .map_err(describe)?;
            for object in page.contents() {
                let modified = object.last_modified().map(DateTime::secs).unwrap_or_default();
                let size = object.size().unwrap_or_default() as u64;
                objects.push(StoredObject { key: object.key().unwrap_or_default().to_string(), modified, size });
            }
            token = page.next_continuation_token().map(Into::into);
            if token.is_none() {
                return Ok(objects);
            }
        }
    }

    pub async fn open_uploads(&self) -> Result<Vec<OpenUpload>> {
        let mut uploads = Vec::new();
        let (mut key_marker, mut id_marker) = (None, None);
        loop {
            let page = self
                .client
                .list_multipart_uploads()
                .bucket(&self.bucket)
                .set_key_marker(key_marker)
                .set_upload_id_marker(id_marker)
                .send()
                .await
                .map_err(describe)?;
            for upload in page.uploads() {
                let key = upload.key().unwrap_or_default().to_string();
                let upload_id = upload.upload_id().unwrap_or_default().to_string();
                let started = upload.initiated().map(DateTime::secs).unwrap_or_default();
                let size = self.uploaded_bytes(&key, &upload_id).await?;
                uploads.push(OpenUpload { key, upload_id, started, size });
            }
            if !page.is_truncated().unwrap_or(false) {
                return Ok(uploads);
            }
            key_marker = page.next_key_marker().map(Into::into);
            id_marker = page.next_upload_id_marker().map(Into::into);
        }
    }

    async fn uploaded_bytes(&self, key: &str, upload_id: &str) -> Result<u64> {
        let mut total = 0;
        let mut marker = None;
        loop {
            let page = self
                .client
                .list_parts()
                .bucket(&self.bucket)
                .key(key)
                .upload_id(upload_id)
                .set_part_number_marker(marker)
                .send()
                .await
                .map_err(describe)?;
            total += page.parts().iter().map(|p| p.size().unwrap_or_default() as u64).sum::<u64>();
            if !page.is_truncated().unwrap_or(false) {
                return Ok(total);
            }
            marker = page.next_part_number_marker().map(Into::into);
        }
    }

    pub async fn abort_upload(&self, key: &str, upload_id: &str) -> Result<()> {
        self.client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
            .map_err(describe)?;
        Ok(())
    }

    /// `on_bytes` is told how many bytes just finished.
    pub async fn upload_file(
        &self,
        path: &Path,
        key: &str,
        size: u64,
        cancel: &AtomicBool,
        on_bytes: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<()> {
        if size < MULTIPART_THRESHOLD {
            let body = ByteStream::from_path(path).await.context("Gagal membaca file")?;
            self.client.put_object().bucket(&self.bucket).key(key).body(body).send().await.map_err(describe)?;
            on_bytes(size);
            return Ok(());
        }

        let upload = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(describe)?;
        let upload_id = upload.upload_id().context("R2 tidak mengembalikan upload id")?.to_string();

        let result = self.upload_parts(path, key, &upload_id, size, cancel, on_bytes).await;
        if result.is_err() {
            // Unfinished parts are billed as storage until they are aborted.
            let _ = self
                .client
                .abort_multipart_upload()
                .bucket(&self.bucket)
                .key(key)
                .upload_id(&upload_id)
                .send()
                .await;
        }
        result
    }

    async fn upload_parts(
        &self,
        path: &Path,
        key: &str,
        upload_id: &str,
        size: u64,
        cancel: &AtomicBool,
        on_bytes: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<()> {
        let mut parts = Vec::new();
        let mut offset = 0;
        let mut number = 1;
        while offset < size {
            if cancel.load(Ordering::Relaxed) {
                bail!("Dibatalkan");
            }
            let length = PART_SIZE.min(size - offset);
            let body = ByteStream::read_from()
                .path(path)
                .offset(offset)
                .length(Length::Exact(length))
                .build()
                .await
                .context("Gagal membaca file")?;
            let part = self
                .client
                .upload_part()
                .bucket(&self.bucket)
                .key(key)
                .upload_id(upload_id)
                .part_number(number)
                .content_length(length as i64)
                .body(body)
                .send()
                .await
                .map_err(describe)?;
            parts.push(CompletedPart::builder().part_number(number).set_e_tag(part.e_tag().map(Into::into)).build());
            on_bytes(length);
            offset += length;
            number += 1;
        }
        self.client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(CompletedMultipartUpload::builder().set_parts(Some(parts)).build())
            .send()
            .await
            .map_err(describe)?;
        Ok(())
    }
}
