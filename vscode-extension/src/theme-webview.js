'use strict';

(() => {
  let source;
  let signature;
  let lanes = [];
  let authors = [];
  const allowed = value => typeof value === 'string' && (/^var\(--vscode-terminal-ansi(?:Bright)?(?:Black|Red|Green|Yellow|Blue|Magenta|Cyan|White)\)$/.test(value) ||
    (/^rgb\((?:\d{1,3}, ){2}\d{1,3}\)$/.test(value) && value.match(/\d+/g).every(channel => Number(channel) <= 255)) || value === 'currentColor');
  const rgb = value => {
    const match = /^rgba?\((\d+),\s*(\d+),\s*(\d+)/.exec(value);
    return match ? match.slice(1).map(Number) : undefined;
  };
  const luminance = values => values.map(value => {
    const c = value / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  }).reduce((total, value, index) => total + value * [0.2126, 0.7152, 0.0722][index], 0);
  const contrast = (a, b) => {
    const x = luminance(a), y = luminance(b);
    return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
  };
  function adjusted(value, background, foreground) {
    if (value === 'currentColor') return value;
    const probe = document.createElement('span');
    probe.style.color = value;
    document.body.appendChild(probe);
    const target = rgb(getComputedStyle(probe).color);
    probe.remove();
    if (!target || !background || !foreground) return value;
    for (let amount = 0; amount <= 100; amount++) {
      const mixed = target.map((channel, index) => Math.round(channel + (foreground[index] - channel) * amount / 100));
      if (contrast(mixed, background) >= 4.5) return `rgb(${mixed.join(', ')})`;
    }
    return `rgb(${foreground.join(', ')})`;
  }
  function apply() {
    if (!source) return;
    const body = getComputedStyle(document.body);
    const bg = rgb(body.backgroundColor);
    const fg = rgb(body.color);
    const highContrast = document.body.classList.contains('vscode-high-contrast') ||
      document.body.classList.contains('vscode-high-contrast-light');
    const convert = value => highContrast ? 'currentColor' : adjusted(value, bg, fg);
    for (const [key, value] of Object.entries(source.values)) {
      document.documentElement.style.setProperty(`--gitpane-${key}`, convert(value));
    }
    lanes = source.lanes.map(convert);
    authors = source.authors.map(convert);
    window.dispatchEvent(new Event('gitpane-theme'));
  }
  function set(value) {
    if (!value || !value.values || typeof value.values !== 'object' || Array.isArray(value.values) ||
        !Array.isArray(value.lanes) || !Array.isArray(value.authors) ||
        !Object.entries(value.values).every(([key, color]) => /^[a-z][a-z0-9-]*$/.test(key) && allowed(color)) ||
        !value.lanes.every(allowed) || !value.authors.every(allowed)) return false;
    const nextSignature = JSON.stringify(value);
    if (nextSignature === signature) return true;
    source = value;
    signature = nextSignature;
    apply();
    return true;
  }
  function clear() {
    if (source) for (const key of Object.keys(source.values)) document.documentElement.style.removeProperty(`--gitpane-${key}`);
    source = undefined;
    signature = undefined;
    lanes = [];
    authors = [];
    window.dispatchEvent(new Event('gitpane-theme'));
  }
  function author(name) {
    if (!authors.length) return 'currentColor';
    let hash = 2166136261;
    for (const byte of new TextEncoder().encode(name)) hash = Math.imul(hash ^ byte, 16777619) >>> 0;
    return authors[hash % authors.length];
  }
  window.GitpaneTheme = { set, clear, lane: index => lanes.length ? lanes[(index % 6) % lanes.length] : 'currentColor', author };
  new MutationObserver(apply).observe(document.body, { attributes: true, attributeFilter: ['class'] });
})();
