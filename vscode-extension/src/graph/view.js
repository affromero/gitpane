'use strict';

const vscode = acquireVsCodeApi();
const graph = document.getElementById('graph');
const search = document.getElementById('search');
const searchStatus = document.getElementById('search-status');
const searchPrev = document.getElementById('search-prev');
const searchNext = document.getElementById('search-next');
const graphStatus = document.getElementById('graph-status');
const firstParent = document.getElementById('first-parent');
const filters = document.getElementById('filters');
const refresh = document.getElementById('refresh');
const collapse = document.getElementById('collapse');
const expandAll = document.getElementById('expand-all');
const filesHeading = document.getElementById('files-heading');
const filesStatus = document.getElementById('files-status');
const files = document.getElementById('files');
const fileSearch = document.getElementById('file-search');
const state = vscode.getState() || {};
let rows = [];
let active = 0;
let selected;
let detail;
let pendingSegment;
let labelMaxLen = 24;

if (typeof state.search === 'string') search.value = state.search;

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = String(text);
  return node;
}

function add(parent, className, text) {
  const node = element('span', className, text);
  parent.appendChild(node);
  return node;
}

const glyphs = {
  empty: ' ', straight: '│', commit: '●', merge_left: '╯', merge_right: '╰',
  fork_left: '╮', fork_right: '╭', horizontal: '─', cross_horizontal: '┼',
  right_tee: '├', left_tee: '┤'
};
const horizontal = new Set(['horizontal', 'cross_horizontal', 'right_tee', 'left_tee']);

function spanAt(row, column, includeEnd) {
  return (row.horizontal_spans || []).find(span =>
    span[0] <= column && (includeEnd ? column <= span[1] : column < span[1]));
}

// The service supplies lane segments and horizontal spans from GraphBuilder.
// This only maps those segments to the glyphs and palette used by the TUI.
function addLanes(container, row) {
  const lanes = Array.isArray(row.lanes) ? row.lanes : [];
  const laneGroup = element('span', 'lanes');
  laneGroup.setAttribute('aria-hidden', 'true');
  for (let column = 0; column < lanes.length; column++) {
    const segment = lanes[column];
    const crossing = horizontal.has(segment) ? spanAt(row, column, true) : undefined;
    const color = crossing ? crossing[2] : column;
    const lane = element('span', 'lane');
    add(lane, `lane-${color % 6}`, glyphs[segment] || ' ');
    const continuation = spanAt(row, column, false);
    add(lane, continuation ? `lane-${continuation[2] % 6}` : '', continuation ? '─' : ' ');
    laneGroup.appendChild(lane);
  }
  container.appendChild(laneGroup);
}

function addLabels(container, row) {
  if (!Array.isArray(row.labels) || !row.labels.length) return;
  const labels = element('span', 'labels');
  add(labels, 'parenthesis', '(');
  row.labels.forEach((label, index) => {
    if (index) add(labels, 'parenthesis', ', ');
    const kind = label.is_stash ? 'stash' : label.is_head ? 'head' : label.is_worktree ? 'worktree' :
      label.is_tag ? 'tag' : label.is_remote ? 'remote' : 'local';
    const prefix = label.is_stash ? '$ ' : label.is_head ? '* ' : label.is_worktree ? '⌂ ' : '';
    const name = Array.from(label.name);
    const displayed = name.length > labelMaxLen ? `${name.slice(0, labelMaxLen).join('')}…` : label.name;
    const node = add(labels, kind, `${prefix}${displayed}`);
    node.title = label.name;
  });
  add(labels, 'parenthesis', ')');
  container.appendChild(labels);
}

function matchRow(row, query) {
  if (!query) return true;
  if (row.collapsed) return false;
  const values = [row.short_id, row.oid, row.message, row.author,
    ...(Array.isArray(row.labels) ? row.labels.map(label => label.name) : [])];
  return values.some(value => String(value || '').toLocaleLowerCase().includes(query));
}

function accessibleRow(row) {
  if (row.collapsed) return `${row.message}, collapsed branch, ${row.collapsed.count} commits`;
  const labels = (row.labels || []).map(label => label.name).join(', ');
  return [row.short_id, row.is_merge ? 'merge commit' : 'commit', row.message,
    row.author, labels].filter(Boolean).join(', ');
}

function renderGraph() {
  const query = search.value.trim().toLocaleLowerCase();
  const matchCount = rows.filter(row => !row.collapsed && matchRow(row, query)).length;
  searchPrev.disabled = searchNext.disabled = !query || !matchCount;
  const selectedRow = rows.find(row => row.oid === selected);
  collapse.disabled = !selectedRow?.segment_id;
  collapse.textContent = selectedRow?.collapsed ? 'Expand branch' : 'Collapse branch';
  searchStatus.textContent = query ? `${matchCount} ${matchCount === 1 ? 'match' : 'matches'}` : '';
  graph.replaceChildren();
  rows.forEach((row, index) => {
    const item = element('div', `commit${index === active ? ' active' : ''}${matchRow(row, query) ? '' : ' nonmatch'}`);
    item.id = `commit-${index}`;
    item.setAttribute('role', 'option');
    item.setAttribute('aria-label', accessibleRow(row));
    item.setAttribute('aria-selected', row.oid === selected ? 'true' : 'false');
    addLanes(item, row);
    if (row.segment_id) add(item, 'branch-toggle', row.collapsed ? '▸' : '▾');
    add(item, 'hash', row.short_id);
    addLabels(item, row);
    add(item, 'summary', row.message);
    if (row.author) add(item, 'author', `· ${row.author}`);
    if (Number.isFinite(row.time)) {
      const time = new Date(row.time * 1000);
      if (!Number.isNaN(time.valueOf())) {
        const timeNode = add(item, 'time', time.toLocaleDateString());
        timeNode.title = time.toLocaleString();
      }
    }
    if (row.diff_stat) {
      const stats = element('span', 'stats');
      add(stats, 'addition', `+${row.diff_stat.additions}`);
      stats.appendChild(document.createTextNode(' '));
      add(stats, 'deletion', `−${row.diff_stat.deletions}`);
      item.appendChild(stats);
    }
    item.addEventListener('click', () => select(index));
    graph.appendChild(item);
  });
  if (rows.length) graph.setAttribute('aria-activedescendant', `commit-${active}`);
  else graph.removeAttribute('aria-activedescendant');
}

function statusClass(status) {
  return ({ M: 'modified', A: 'added', D: 'deleted', R: 'renamed', '?': 'untracked', C: 'conflicted' })[status] || 'untracked';
}

function renderFiles() {
  files.replaceChildren();
  fileSearch.disabled = true;
  const selectedRow = rows.find(row => row.oid === selected);
  filesHeading.textContent = selectedRow ? `Files · ${selectedRow.short_id}` : 'Commit files';
  filesStatus.className = 'message';
  if (selectedRow?.collapsed) {
    filesHeading.textContent = 'Commit files';
    filesStatus.textContent = 'Expand the branch to select a commit.';
    return;
  }
  if (!selectedRow) {
    filesStatus.textContent = 'Select a commit to see its files.';
    return;
  }
  if (!detail || detail.oid !== selected || detail.loading) {
    filesStatus.textContent = 'Loading files…';
    return;
  }
  if (detail.error) {
    filesStatus.className = 'message error';
    filesStatus.textContent = detail.error;
    return;
  }
  const entries = Array.isArray(detail.files) ? detail.files : [];
  fileSearch.disabled = !entries.length;
  const query = fileSearch.value.trim().toLocaleLowerCase();
  const matches = entries.filter(entry => String(entry.path || '').toLocaleLowerCase().includes(query));
  filesStatus.textContent = !entries.length ? 'No files changed.' : !query ? '' :
    matches.length ? `${matches.length} ${matches.length === 1 ? 'file matches' : 'files match'}` : 'No matching files.';
  for (const entry of matches) {
    const listItem = element('li');
    const button = element('button', 'file');
    button.type = 'button';
    button.setAttribute('aria-label', `${entry.status || 'changed'} ${entry.path}`);
    add(button, `file-status ${statusClass(entry.status)}`, entry.status || '?');
    const filePath = add(button, 'file-path', entry.path);
    filePath.title = entry.path;
    button.addEventListener('click', () => vscode.postMessage({ type: 'openCommitFile', id: entry.id }));
    listItem.appendChild(button);
    files.appendChild(listItem);
  }
}

function select(index, focus = true) {
  if (!rows[index]) return;
  if (selected !== rows[index].oid) fileSearch.value = '';
  active = index;
  selected = rows[index].oid;
  detail = undefined;
  renderGraph();
  renderFiles();
  if (focus) graph.focus();
  document.getElementById(`commit-${active}`)?.scrollIntoView({ block: 'nearest' });
  if (!rows[index].collapsed) vscode.postMessage({ type: 'selectCommit', oid: selected });
}

function navigateMatches(direction) {
  const query = search.value.trim().toLocaleLowerCase();
  if (!query) return;
  const matches = rows.map((row, index) => !row.collapsed && matchRow(row, query) ? index : -1).filter(index => index >= 0);
  if (!matches.length) return;
  const current = matches.findIndex(index => rows[index].oid === selected);
  const position = current < 0 ? (direction > 0 ? 0 : matches.length - 1) :
    (current + direction + matches.length) % matches.length;
  select(matches[position], false);
}

graph.addEventListener('keydown', event => {
  if (!rows.length) return;
  let index;
  if (event.key === 'ArrowDown') index = Math.min(active + 1, rows.length - 1);
  else if (event.key === 'ArrowUp') index = Math.max(active - 1, 0);
  else if (event.key === 'Home') index = 0;
  else if (event.key === 'End') index = rows.length - 1;
  else if (event.key === 'Enter' || event.key === ' ') index = active;
  else return;
  event.preventDefault();
  select(index);
});

search.addEventListener('input', () => {
  vscode.setState({ ...vscode.getState(), search: search.value });
  renderGraph();
});
search.addEventListener('keydown', event => {
  if (event.key !== 'Enter') return;
  event.preventDefault();
  navigateMatches(event.shiftKey ? -1 : 1);
});
searchPrev.addEventListener('click', () => navigateMatches(-1));
searchNext.addEventListener('click', () => navigateMatches(1));
fileSearch.addEventListener('input', renderFiles);
fileSearch.addEventListener('keydown', event => {
  if (event.key !== 'Enter' && event.key !== 'ArrowDown') return;
  const first = files.querySelector('.file');
  if (!first) return;
  event.preventDefault();
  first.focus();
});

firstParent.addEventListener('change', () => {
  vscode.postMessage({ type: 'firstParent', value: firstParent.checked });
});

refresh.addEventListener('click', () => vscode.postMessage({ type: 'refresh' }));
filters.addEventListener('click', () => vscode.postMessage({ type: 'openFilters' }));
collapse.addEventListener('click', () => {
  const row = rows.find(item => item.oid === selected);
  if (!row?.segment_id) return;
  pendingSegment = { id: row.segment_id, expand: Boolean(row.collapsed) };
  vscode.postMessage({ type: 'toggleCollapse', id: row.segment_id });
});
expandAll.addEventListener('click', () => {
  const row = rows.find(item => item.oid === selected);
  pendingSegment = row?.segment_id ? { id: row.segment_id, expand: Boolean(row.collapsed) } : undefined;
  vscode.postMessage({ type: 'expandAll' });
});

window.addEventListener('message', event => {
  const message = event.data;
  if (message?.type === 'graph') {
    rows = Array.isArray(message.rows) ? message.rows : [];
    labelMaxLen = Number.isSafeInteger(message.labelMaxLen) && message.labelMaxLen >= 0 ? message.labelMaxLen : 24;
    expandAll.disabled = !message.collapsedCount || message.loading;
    firstParent.checked = Boolean(message.firstParent);
    const activeFilters = (Array.isArray(message.filters?.branches) ? 1 : 0) +
      (Array.isArray(message.filters?.authors) ? 1 : 0) +
      Object.values(message.filters?.refs || {}).filter(value => value === false).length;
    filters.textContent = activeFilters ? `Filters (${activeFilters})` : 'Filters';
    filters.setAttribute('aria-label', activeFilters ? `Filter commit graph, ${activeFilters} active` : 'Filter commit graph');
    let openExpandedCommit = false;
    if (!message.loading && pendingSegment) {
      const replacement = rows.find(row => row.segment_id === pendingSegment.id);
      if (replacement) selected = replacement.oid;
      openExpandedCommit = Boolean(replacement && !replacement.collapsed && pendingSegment.expand);
      pendingSegment = undefined;
    }
    if (!rows.some(row => row.oid === selected)) {
      selected = undefined;
      detail = undefined;
    }
    active = Math.max(0, rows.findIndex(row => row.oid === selected));
    graphStatus.className = message.error ? 'message error' : 'message';
    const limitReached = Number.isSafeInteger(message.limit) && message.limit > 0 && rows.length >= message.limit;
    graphStatus.textContent = message.error || (message.loading ? 'Loading graph…' : !rows.length ? 'No commits.' :
      limitReached ? `Showing up to ${message.limit} commits.` : '');
    graph.setAttribute('aria-busy', message.loading ? 'true' : 'false');
    renderGraph();
    renderFiles();
    if (openExpandedCommit) {
      detail = undefined;
      renderFiles();
      vscode.postMessage({ type: 'selectCommit', oid: selected });
    }
  } else if (message?.type === 'commitFiles' && message.oid === selected) {
    detail = message;
    renderFiles();
  }
});

renderGraph();
renderFiles();
vscode.postMessage({ type: 'ready' });
