'use strict';

window.GitpaneColumns = (document, window, tree, stored, save) => {
  const valid = value => Array.isArray(value) && value.length === 2 &&
    value.every(number => Number.isFinite(number) && number >= 24 && number <= 2000);
  let preferred = valid(stored) ? [...stored] : null;
  let displayed = [0, 0];
  const layout = document.createElement('div');
  layout.id = 'column-layout';
  tree.before(layout);
  const header = document.createElement('div');
  header.className = 'column-header';
  const marker = document.createElement('span');
  const names = ['Name', 'Branch', 'Status'];
  const cells = names.map(name => {
    const cell = document.createElement('span');
    const label = document.createElement('span');
    label.className = 'column-label';
    label.textContent = name;
    cell.appendChild(label);
    return cell;
  });
  const reset = document.createElement('button');
  reset.className = 'column-reset';
  reset.type = 'button';
  reset.textContent = '↺';
  reset.title = 'Reset column widths';
  reset.setAttribute('aria-label', reset.title);
  cells[2].append(' ', reset);
  header.append(marker, ...cells);
  layout.append(header, tree);
  const handles = cells.slice(0, 2).map((cell, index) => {
    const handle = document.createElement('span');
    handle.className = 'column-separator';
    handle.tabIndex = 0;
    handle.setAttribute('role', 'separator');
    handle.setAttribute('aria-orientation', 'vertical');
    handle.setAttribute('aria-label', index ? 'Resize Branch column' : 'Resize Name column');
    cell.appendChild(handle);
    return handle;
  });

  function render() {
    const width = layout.getBoundingClientRect().width;
    if (!width) return;
    const markerWidth = marker.getBoundingClientRect().width;
    const gap = parseFloat(window.getComputedStyle(layout).columnGap) || 0;
    const available = Math.max(48, width - markerWidth - gap * 3 - 85);
    if (preferred) {
      const name = preferred[0];
      const branch = preferred[1];
      layout.dataset.resized = 'true';
      layout.style.setProperty('--column-name', `${name}px`);
      layout.style.setProperty('--column-branch', `${branch}px`);
    } else {
      delete layout.dataset.resized;
      layout.style.removeProperty('--column-name');
      layout.style.removeProperty('--column-branch');
    }
    displayed = cells.slice(0, 2).map(cell => cell.getBoundingClientRect().width);
    handles.forEach((handle, index) => {
      handle.setAttribute('aria-valuemin', '24');
      handle.setAttribute('aria-valuemax', String(Math.round(Math.max(24, available - displayed[1 - index]))));
      handle.setAttribute('aria-valuenow', String(Math.round(displayed[index])));
    });
  }
  function resize(index, delta) {
    displayed = cells.slice(0, 2).map(cell => cell.getBoundingClientRect().width);
    preferred = preferred ? [...preferred] : displayed.map(value => Math.max(24, value));
    preferred[index] = Math.max(24, Math.min(2000, displayed[index] + delta));
    render();
    save();
  }
  handles.forEach((handle, index) => {
    handle.addEventListener('pointerdown', event => {
      if (event.button !== 0) return;
      event.preventDefault();
      handle.setPointerCapture(event.pointerId);
      let previous = event.clientX;
      const move = next => { resize(index, next.clientX - previous); previous = next.clientX; };
      const stop = () => { handle.removeEventListener('pointermove', move); handle.removeEventListener('pointerup', stop); handle.removeEventListener('pointercancel', stop); };
      handle.addEventListener('pointermove', move);
      handle.addEventListener('pointerup', stop);
      handle.addEventListener('pointercancel', stop);
    });
    handle.addEventListener('keydown', event => {
      const delta = event.key === 'ArrowLeft' ? -10 : event.key === 'ArrowRight' ? 10 : 0;
      if (!delta) return;
      event.preventDefault();
      resize(index, delta);
    });
  });
  reset.addEventListener('click', () => { preferred = null; render(); save(); });
  new ResizeObserver(render).observe(layout);
  render();
  return { widths: () => preferred && [...preferred] };
};
