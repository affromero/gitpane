'use strict';

window.GitpaneComparisonView = function ({ document, post, renderGraph }) {
  let state;
  const section = document.createElement('section');
  section.className = 'comparison';
  section.hidden = true;
  section.setAttribute('aria-label', 'Revision comparison');
  const target = document.createElement('div');
  target.className = 'comparison-target';
  const bar = document.createElement('div');
  bar.className = 'comparison-bar';
  const status = document.createElement('div');
  status.setAttribute('role', 'status');
  status.setAttribute('aria-live', 'polite');
  const from = document.createElement('button');
  const to = document.createElement('button');
  const fromSelected = document.createElement('button');
  const toSelected = document.createElement('button');
  const browseFrom = document.createElement('button');
  const browseTo = document.createElement('button');
  const swap = document.createElement('button');
  const clear = document.createElement('button');
  const refresh = document.createElement('button');
  for (const [button, label] of [[browseFrom, 'Browse From refs'], [browseTo, 'Browse To refs'],
    [fromSelected, 'Set selected as From'], [toSelected, 'Set selected as To'],
    [swap, 'Swap'], [clear, 'Clear'], [refresh, 'Refresh refs']]) {
    button.type = 'button'; button.textContent = label; bar.appendChild(button);
  }
  from.type = to.type = 'button';
  bar.prepend(from, to);
  const modeLabel = document.createElement('label');
  modeLabel.textContent = 'Comparison mode ';
  const mode = document.createElement('select');
  for (const [value, label] of [['endpoints', 'Compare endpoints'], ['common_ancestor', 'Changes since common ancestor']]) {
    const option = document.createElement('option'); option.value = value; option.textContent = label; mode.appendChild(option);
  }
  modeLabel.appendChild(mode);
  bar.appendChild(modeLabel);
  const heading = document.createElement('h2'); heading.textContent = 'Comparison files';
  const searchLabel = document.createElement('label'); searchLabel.textContent = 'Search comparison files ';
  const search = document.createElement('input'); search.type = 'search'; searchLabel.appendChild(search);
  const files = document.createElement('ul'); files.className = 'files';
  const results = document.createElement('section');
  results.className = 'comparison-results'; results.hidden = true;
  results.setAttribute('aria-label', 'Comparison files');
  section.append(target, bar, status);
  results.append(heading, searchLabel, files);
  const graph = document.getElementById('graph');
  graph.before(section); graph.after(results);
  from.addEventListener('click', () => post({ type: 'comparisonPick', which: 'from' }));
  to.addEventListener('click', () => post({ type: 'comparisonPick', which: 'to' }));
  browseFrom.addEventListener('click', () => post({ type: 'comparisonBrowse', which: 'from' }));
  browseTo.addEventListener('click', () => post({ type: 'comparisonBrowse', which: 'to' }));
  fromSelected.addEventListener('click', () => post({ type: 'comparisonSetSelected', which: 'from' }));
  toSelected.addEventListener('click', () => post({ type: 'comparisonSetSelected', which: 'to' }));
  swap.addEventListener('click', () => post({ type: 'comparisonSwap' }));
  clear.addEventListener('click', () => post({ type: 'comparisonClear' }));
  refresh.addEventListener('click', () => post({ type: 'comparisonRefresh' }));
  mode.addEventListener('change', () => post({ type: 'comparisonMode', mode: mode.value }));
  search.addEventListener('input', renderFiles);

  function renderFiles() {
    files.replaceChildren();
    const query = search.value.toLocaleLowerCase();
    for (const file of state?.files || []) {
      if (!`${file.path} ${file.old_path || ''}`.toLocaleLowerCase().includes(query)) continue;
      const item = document.createElement('li');
      const button = document.createElement('button');
      button.type = 'button'; button.className = 'file';
      const text = file.old_path ? `${file.old_path} → ${file.path}` : file.path;
      button.textContent = `${file.status} ${text}${file.is_submodule ? ' (submodule)' : ''}`;
      button.addEventListener('click', () => post({ type: 'openComparisonFile', id: file.id }));
      item.appendChild(button); files.appendChild(item);
    }
  }

  function label(which, endpoint) {
    if (!endpoint) return `${which}: select revision`;
    return `${which}: ${endpoint.revision} (${endpoint.oid.slice(0, 8)})${endpoint.moved ? ' · moved' : ''}${endpoint.error ? ' · unavailable' : ''}`;
  }

  function receive(value) {
    if (value?.type !== 'comparisonState') return false;
    const filesChanged = JSON.stringify(state?.files) !== JSON.stringify(value.files);
    state = value;
    section.hidden = results.hidden = false;
    document.body.classList.add('comparing');
    const dirty = value.dirty === true ? 'Uncommitted changes excluded' : value.dirty === false ? 'Clean working tree' : 'Working tree status unavailable';
    target.textContent = [value.repoName, value.repoPath, value.head, dirty].filter(Boolean).join(' · ');
    from.textContent = label('From', value.from); to.textContent = label('To', value.to);
    mode.value = value.mode || 'endpoints';
    status.textContent = value.error || (value.loading ? 'Comparing revisions…' : value.from && value.to ?
      (!value.hasResult ? 'Comparison incomplete. Select Refresh refs to retry.' :
        value.files?.length ? `${value.files.length} changed files` : 'No files changed.') : 'Select two revisions.');
    swap.disabled = clear.disabled = refresh.disabled = !!value.loading;
    if (filesChanged) renderFiles();
    renderGraph();
    return true;
  }

  function decorate(item, row) {
    if (!state || row.collapsed) return;
    for (const [name, endpoint] of [['From', state.from], ['To', state.to]]) {
      if (endpoint?.oid !== row.oid) continue;
      const badge = document.createElement('span'); badge.className = `comparison-${name.toLowerCase()}`;
      badge.textContent = name; item.appendChild(badge);
      item.setAttribute('aria-label', `${item.getAttribute('aria-label')}, ${name} endpoint`);
    }
  }

  return { receive, decorate, enabled: () => !!state };
};
