'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const { color, validateTheme, ThemeController } = require('../src/theme');

const names = {
  repo_list: ['dirty_marker', 'branch', 'branch_default', 'ahead', 'behind', 'worktree_count', 'dirty_submodule',
    'unpushed_submodule', 'stash', 'file_count', 'repo_name', 'worktree_subtree_icon', 'worktree_subtree_branch', 'live'],
  file_list: ['status_modified', 'status_added', 'status_deleted', 'status_renamed', 'status_untracked',
    'status_conflicted', 'submodule_path', 'regular_path'],
  graph: ['commit_id', 'commit_message', 'merge_message', 'time', 'addition', 'deletion', 'paren', 'head_marker',
    'worktree_marker', 'tag_label', 'remote_label', 'local_branch_label', 'stash_label', 'dimmed',
    'commit_files_status_modified', 'commit_files_status_added', 'commit_files_status_deleted',
    'commit_files_status_renamed', 'commit_files_status_other', 'commit_files_path']
};
function fixture() {
  const result = { name: 'custom' };
  for (const [section, fields] of Object.entries(names)) {
    result[section] = Object.fromEntries(fields.map(field => [field, { type: 'reset' }]));
  }
  Object.assign(result.graph, { lane_palette: [], author_palette: [] });
  return result;
}

test('both production webviews load their theme script within the allowed source root', () => {
  const load = Module._load;
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return { Uri: { joinPath: (...parts) => parts.join('/') } };
      return load.call(this, name, ...args);
    };
    const webview = { cspSource: 'vscode-resource:', asWebviewUri: value => value };
    for (const file of ['../src/webview/html', '../src/graph/html']) {
      delete require.cache[require.resolve(file)];
      const html = require(file).getHtml(webview, '/extension');
      assert.match(html, /src="\/extension\/src\/theme-webview\.js"/);
      assert.match(html, /style-src vscode-resource:; script-src vscode-resource:/);
      assert.doesNotMatch(html, /unsafe-inline/);
      delete require.cache[require.resolve(file)];
    }
  } finally { Module._load = load; }
});

test('native colors map to safe ANSI, indexed, RGB and inherited colors', () => {
  assert.equal(color({ type: 'ansi', name: 'light_blue' }), 'var(--vscode-terminal-ansiBrightBlue)');
  assert.equal(color({ type: 'indexed', value: 0 }), 'var(--vscode-terminal-ansiBlack)');
  assert.equal(color({ type: 'indexed', value: 15 }), 'var(--vscode-terminal-ansiBrightWhite)');
  assert.equal(color({ type: 'indexed', value: 214 }), 'rgb(255, 175, 0)');
  assert.equal(color({ type: 'indexed', value: 255 }), 'rgb(238, 238, 238)');
  assert.equal(color({ type: 'rgb', red: 1, green: 2, blue: 3 }), 'rgb(1, 2, 3)');
  assert.equal(color({ type: 'reset' }), 'currentColor');
  for (const invalid of [{ type: 'ansi', name: 'red);color:red' }, { type: 'rgb', red: -1, green: 2, blue: 3 },
    { type: 'indexed', value: 256 }, { type: 'reset', extra: true }, { type: 'rgb', red: '1', green: 2, blue: 3 }]) {
    assert.throws(() => color(invalid), /invalid theme color/i);
  }
  const theme = fixture();
  theme.repo_list.stash = { type: 'indexed', value: 127 };
  theme.graph.lane_palette = [{ type: 'ansi', name: 'cyan' }, { type: 'rgb', red: 1, green: 2, blue: 3 }];
  const converted = validateTheme(theme);
  assert.equal(converted.values['repo-list-stash'], 'rgb(175, 0, 175)');
  assert.deepEqual(converted.lanes, ['var(--vscode-terminal-ansiCyan)', 'rgb(1, 2, 3)']);
  assert.deepEqual(converted.authors, []);
  theme.file_list.status_added = { type: 'ansi', name: 'red;display:none' };
  assert.throws(() => validateTheme(theme), /invalid theme color/i);
});

test('theme reloads on backend generation changes, reports errors and ignores stale responses', async () => {
  let notify;
  let finish;
  let reply = () => Promise.resolve(fixture());
  const backend = { generation: 1, state: { status: 'initializing' },
    onDidChange(handler) { notify = handler; return { dispose() {} }; }, theme: () => reply() };
  const controller = new ThemeController(backend);
  try {
    reply = () => new Promise(resolve => { finish = resolve; });
    notify({ status: 'ready' });
    backend.generation = 2;
    reply = () => Promise.reject(new Error('custom theme missing'));
    notify({ status: 'ready' });
    await new Promise(resolve => setImmediate(resolve));
    assert.match(controller.error, /custom theme missing/);
    finish(fixture());
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(controller.value, undefined);
    backend.generation = 3;
    reply = () => Promise.resolve(fixture());
    notify({ status: 'ready' });
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(controller.value.name, 'custom');
    assert.equal(controller.error, undefined);
  } finally { controller.dispose(); }
});

test('reconfiguration clears stale themes and an explicit retry recovers within the same generation', async () => {
  let notify;
  let colorChanged;
  let finish;
  let reply = () => Promise.resolve(fixture());
  const backend = { generation: 1, state: { status: 'initializing' },
    onDidChange(handler) { notify = handler; return { dispose() {} }; }, theme: () => reply() };
  const controller = new ThemeController(backend, handler => { colorChanged = handler; return { dispose() {} }; });
  try {
    notify({ status: 'ready' });
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(controller.value.name, 'custom');
    const previousAppearance = controller.payload().appearance;
    colorChanged();
    assert.equal(controller.payload().appearance, previousAppearance + 1, 'same-class VS Code theme changes trigger reapplication');

    backend.generation = 2;
    notify({ status: 'initializing' });
    assert.equal(controller.value, undefined, 'old theme is cleared as reconfiguration starts');
    reply = () => new Promise(resolve => { finish = resolve; });
    notify({ status: 'ready' });
    notify({ status: 'error', error: new Error('backend initialization failed') });
    finish(fixture());
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(controller.value, undefined);
    assert.match(controller.error, /backend initialization failed/);

    reply = () => Promise.reject(new Error('theme temporarily unavailable'));
    await controller.retry();
    assert.match(controller.error, /theme temporarily unavailable/);
    reply = () => Promise.resolve(fixture());
    await controller.retry();
    assert.equal(controller.value.name, 'custom');
    assert.equal(controller.error, undefined);
  } finally { controller.dispose(); }
});
