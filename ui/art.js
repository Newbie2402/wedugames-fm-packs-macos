// Built-in card art for packs that have no cover image yet. Everything is
// generic: no real players, clubs or brands.

function seedOf(text) {
  let seed = 7;
  for (const char of text) seed = (seed * 31 + char.charCodeAt(0)) >>> 0;
  return seed;
}

function backdrop(id, hue) {
  return `
    <defs>
      <linearGradient id="bg-${id}" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0" stop-color="hsl(${hue} 62% 30%)" />
        <stop offset="1" stop-color="hsl(${(hue + 40) % 360} 58% 12%)" />
      </linearGradient>
      <radialGradient id="glow-${id}" cx="0.5" cy="0.42" r="0.6">
        <stop offset="0" stop-color="hsl(${hue} 90% 66%)" stop-opacity="0.55" />
        <stop offset="1" stop-color="hsl(${hue} 90% 66%)" stop-opacity="0" />
      </radialGradient>
    </defs>
    <rect width="320" height="200" fill="url(#bg-${id})" />
    <g stroke="#fff" stroke-opacity="0.07" stroke-width="16">
      <path d="M-20 60 L120 -40M40 240 L300 -40M170 240 L360 40" />
    </g>
    <rect width="320" height="200" fill="url(#glow-${id})" />`;
}

function person(id, skinTop, skinBottom, shirt) {
  return `
    <defs>
      <linearGradient id="skin-${id}" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0" stop-color="${skinTop}" />
        <stop offset="1" stop-color="${skinBottom}" />
      </linearGradient>
    </defs>
    <path d="M58 200 C62 156 112 142 160 142 C208 142 258 156 262 200 Z" fill="${shirt}" />
    <path d="M136 124 H184 V150 C176 164 144 164 136 150 Z" fill="url(#skin-${id})" />
    <ellipse cx="160" cy="88" rx="38" ry="48" fill="url(#skin-${id})" />
    <path d="M120 92 C106 22 214 22 200 92 C196 74 184 66 160 66 C136 66 124 74 120 92 Z" fill="#141a20" />`;
}

function facesArt(plan, id, hue) {
  const name = plan.name.toLowerCase();
  const figure = person(id, "#f0d9c3", "#b89476", `hsl(${(hue + 170) % 360} 62% 42%)`);
  if (name.includes("site") || (!name.includes("cut") && !name.includes("df") && seedOf(id) % 2)) {
    return `${backdrop(id, hue)}
      <defs><clipPath id="hex-${id}"><path d="M160 10 L258 54 V146 L160 190 L62 146 V54 Z" /></clipPath></defs>
      <g clip-path="url(#hex-${id})"><rect width="320" height="200" fill="#e9eef2" />${figure}</g>
      <path d="M160 10 L258 54 V146 L160 190 L62 146 V54 Z" fill="none" stroke="#fff" stroke-width="5" />
      <path d="M160 2 L266 49 V151 L160 198 L54 151 V49 Z" fill="none" stroke="hsl(${hue} 80% 70%)" stroke-width="2" />`;
  }
  if (name.includes("df")) {
    return `${backdrop(id, hue)}
      <defs><clipPath id="frame-${id}"><rect x="96" y="14" width="128" height="172" rx="10" /></clipPath></defs>
      <rect x="88" y="6" width="144" height="188" rx="16" fill="hsl(${hue} 50% 40%)" />
      <g clip-path="url(#frame-${id})"><rect x="96" y="14" width="128" height="172" fill="#dfe5ea" />
        <g transform="translate(32 18) scale(0.8)">${figure}</g></g>`;
  }
  return `${backdrop(id, hue)}${figure}`;
}

const CREST_MATERIALS = {
  steel: ["#eef2f5", "#7f8c97", "#3a444d"],
  metal: ["#ffe9a8", "#d3a032", "#6f4c0c"],
  plastic: ["#ff8d7a", "#e0392c", "#8a130c"],
};

function logosArt(plan, id, hue) {
  const name = plan.name.toLowerCase();
  const material = name.includes("steel") ? CREST_MATERIALS.steel
    : name.includes("metal") ? CREST_MATERIALS.metal
    : name.includes("plastic") ? CREST_MATERIALS.plastic
    : [`hsl(${hue} 90% 70%)`, `hsl(${hue} 76% 46%)`, `hsl(${hue} 80% 20%)`];
  const emblems = [
    `<circle cx="160" cy="92" r="30" fill="none" stroke="#fff" stroke-width="5" />
     <path d="M160 74 l17 12 -6 20 h-22 l-6 -20 Z" fill="#fff" />`,
    `<path d="M160 58 l10 22 24 2 -18 16 6 24 -22 -13 -22 13 6 -24 -18 -16 24 -2 Z" fill="#fff" />`,
    `<path d="M118 84 L160 62 L202 84 V104 L160 82 L118 104 Z M118 114 L160 92 L202 114 V134 L160 112 L118 134 Z" fill="#fff" />`,
  ];
  return `${backdrop(id, (hue + 200) % 360)}
    <defs>
      <linearGradient id="crest-${id}" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0" stop-color="${material[0]}" />
        <stop offset="0.55" stop-color="${material[1]}" />
        <stop offset="1" stop-color="${material[2]}" />
      </linearGradient>
    </defs>
    <path d="M160 16 L238 40 V102 C238 146 200 172 160 186 C120 172 82 146 82 102 V40 Z" fill="${material[2]}" />
    <path d="M160 26 L228 47 V102 C228 139 195 162 160 175 C125 162 92 139 92 102 V47 Z" fill="url(#crest-${id})" />
    ${emblems[seedOf(id) % emblems.length]}`;
}

function kitsArt(plan, id, hue) {
  const shirt = "M118 28 L92 36 L50 74 L78 104 L100 88 V184 H220 V88 L242 104 L270 74 L228 36 L202 28 C190 44 130 44 118 28 Z";
  return `${backdrop(id, (hue + 90) % 360)}
    <defs><clipPath id="shirt-${id}"><path d="${shirt}" /></clipPath></defs>
    <path d="${shirt}" fill="#f4f6f8" />
    <g clip-path="url(#shirt-${id})" fill="hsl(${hue} 80% 44%)">
      <rect x="114" y="20" width="26" height="170" /><rect x="180" y="20" width="26" height="170" />
      <rect x="40" y="20" width="60" height="100" /><rect x="220" y="20" width="60" height="100" />
    </g>
    <path d="M118 28 C130 44 190 44 202 28 L194 25 C182 36 138 36 126 25 Z" fill="#141a20" />`;
}

function trophyArt(plan, id, hue) {
  return `${backdrop(id, 36)}
    <defs>
      <linearGradient id="cup-${id}" x1="0" y1="0" x2="1" y2="0">
        <stop offset="0" stop-color="#8d6417" /><stop offset="0.45" stop-color="#ffe6a0" /><stop offset="1" stop-color="#a97a1d" />
      </linearGradient>
    </defs>
    <g fill="none" stroke="#e7bf5c" stroke-width="9">
      <path d="M112 44 C76 44 84 104 128 108" /><path d="M208 44 C244 44 236 104 192 108" />
    </g>
    <path d="M108 26 H212 V76 C212 112 186 128 160 128 C134 128 108 112 108 76 Z" fill="url(#cup-${id})" />
    <rect x="150" y="126" width="20" height="30" fill="url(#cup-${id})" />
    <rect x="120" y="154" width="80" height="28" rx="4" fill="#20262d" />`;
}

function namesArt(plan, id, hue) {
  return `${backdrop(id, 158)}
    <g font-family="Bahnschrift, Segoe UI, sans-serif" font-weight="700">
      <rect x="58" y="44" width="204" height="44" rx="8" fill="#0a0e13" fill-opacity="0.55" />
      <text x="76" y="74" font-size="22" fill="#fff" fill-opacity="0.4" text-decoration="line-through">UNLICENSED</text>
      <rect x="58" y="104" width="204" height="52" rx="8" fill="#f4f6f8" />
      <text x="76" y="139" font-size="24" fill="#0b2a22">REAL NAME</text>
      <circle cx="236" cy="130" r="14" fill="#0fae79" />
      <path d="M229 130 l5 5 9 -10" fill="none" stroke="#fff" stroke-width="3" stroke-linecap="round" />
    </g>`;
}

function packArt(plan) {
  const id = plan.id.replace(/[^a-z0-9-]/g, "");
  const hue = seedOf(plan.id) % 360;
  let inner;
  if (plan.group === "faces") inner = facesArt(plan, id, hue);
  else if (plan.group === "logos") inner = logosArt(plan, id, hue);
  else if (plan.group?.startsWith("kits")) inner = kitsArt(plan, id, hue);
  else if (plan.target === "database") inner = namesArt(plan, id, hue);
  else inner = trophyArt(plan, id, hue);
  return `<svg viewBox="0 0 320 200" preserveAspectRatio="xMidYMid slice" aria-hidden="true">${inner}</svg>`;
}
