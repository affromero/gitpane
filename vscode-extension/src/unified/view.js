'use strict';

(() => {
  const vscode = acquireVsCodeApi();
  const root = document.getElementById('panes');
  const panes = [...root.querySelectorAll('.pane')];
  const splitters = [...root.querySelectorAll('.splitter')];
  const frames = Object.fromEntries([...root.querySelectorAll('iframe')].map(frame => [frame.dataset.surface, frame]));
  const surfaces = new Set(Object.keys(frames));
  const saved = vscode.getState() || {};
  const stored = saved.paneSizes;
  let sizes = Array.isArray(stored) && stored.length === 3 && stored.every(value => Number.isFinite(value) && value > 0)
    ? stored.map(value => value / stored.reduce((a, b) => a + b, 0)) : [0.34, 0.33, 0.33];

  function save(field, value) { vscode.setState({ ...(vscode.getState() || {}), [field]: value }); }
  window.GitpaneFrameApi = surface => {
    if (!surfaces.has(surface)) throw new Error('Unknown Gitpane view.');
    return {
      getState: () => (vscode.getState() || {}).views?.[surface],
      setState: value => { const state = vscode.getState() || {}; save('views', { ...(state.views || {}), [surface]: value }); },
      postMessage: message => vscode.postMessage({ surface, message })
    };
  };
  window.addEventListener('message', event => {
    const { surface, message } = event.data || {};
    if (surfaces.has(surface) && message && !Object.values(frames).some(frame => event.source === frame.contentWindow)) {
      frames[surface].contentWindow?.postMessage(message, '*');
    }
  });

  function syncTheme(frame) {
    const doc = frame.contentDocument;
    if (!doc?.body) return;
    const styles = getComputedStyle(document.body);
    for (const key of styles) if (key.startsWith('--vscode-')) doc.documentElement.style.setProperty(key, styles.getPropertyValue(key));
    const keep = [...doc.body.classList].filter(name => !name.startsWith('vscode-'));
    doc.body.className = [...keep, ...[...document.body.classList].filter(name => name.startsWith('vscode-'))].join(' ');
  }
  for (const frame of Object.values(frames)) {
    frame.addEventListener('load', () => syncTheme(frame));
    frame.srcdoc = frame.dataset.document;
    delete frame.dataset.document;
  }
  new MutationObserver(() => { for (const frame of Object.values(frames)) syncTheme(frame); })
    .observe(document.documentElement, { attributes: true, subtree: true, attributeFilter: ['class', 'style'] });

  function render() {
    panes.forEach((pane, index) => { pane.style.flex = `${sizes[index]} 1 0px`; });
    splitters.forEach((splitter, index) => {
      splitter.setAttribute('aria-valuemin', '0');
      splitter.setAttribute('aria-valuemax', '100');
      splitter.setAttribute('aria-valuenow', String(Math.round(sizes[index] * 100)));
    });
  }
  function resize(index, delta) {
    const height = panes.reduce((sum, pane) => sum + pane.getBoundingClientRect().height, 0);
    if (!height) return;
    const total = sizes[index] + sizes[index + 1];
    const minimum = Math.min(56 / height, total / 2);
    const next = Math.min(total - minimum, Math.max(minimum, sizes[index] + delta / height));
    sizes[index] = next;
    sizes[index + 1] = total - next;
    render();
    save('paneSizes', sizes);
  }
  splitters.forEach((splitter, index) => {
    splitter.addEventListener('pointerdown', event => {
      if (event.button !== 0) return;
      event.preventDefault();
      splitter.setPointerCapture(event.pointerId);
      let previous = event.clientY;
      const move = next => { resize(index, next.clientY - previous); previous = next.clientY; };
      const stop = () => { splitter.removeEventListener('pointermove', move); splitter.removeEventListener('pointerup', stop); splitter.removeEventListener('pointercancel', stop); };
      splitter.addEventListener('pointermove', move);
      splitter.addEventListener('pointerup', stop);
      splitter.addEventListener('pointercancel', stop);
    });
    splitter.addEventListener('keydown', event => {
      const delta = event.key === 'ArrowUp' ? -10 : event.key === 'ArrowDown' ? 10 : 0;
      if (!delta) return;
      event.preventDefault();
      resize(index, delta);
    });
  });
  render();
})();
