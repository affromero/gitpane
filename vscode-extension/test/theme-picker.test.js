'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { ThemeController } = require('../src/theme');
const { ThemePicker } = require('../src/theme-picker');

const sections = {
  repo_list: ['dirty_marker', 'branch', 'branch_default', 'ahead', 'behind', 'worktree_count', 'dirty_submodule', 'unpushed_submodule', 'stash', 'file_count', 'repo_name', 'worktree_subtree_icon', 'worktree_subtree_branch', 'live'],
  file_list: ['status_modified', 'status_added', 'status_deleted', 'status_renamed', 'status_untracked', 'status_conflicted', 'submodule_path', 'regular_path',
    'submodule_bracket', 'submodule_branch', 'submodule_unpushed', 'submodule_unreachable', 'submodule_needs_merge'],
  graph: ['commit_id', 'commit_message', 'merge_message', 'time', 'addition', 'deletion', 'paren', 'head_marker', 'worktree_marker', 'tag_label', 'remote_label', 'local_branch_label', 'stash_label', 'dimmed', 'commit_files_status_modified', 'commit_files_status_added', 'commit_files_status_deleted', 'commit_files_status_renamed', 'commit_files_status_other', 'commit_files_path']
};
function theme(name) {
  const value = { name };
  for (const [section, names] of Object.entries(sections)) value[section] = Object.fromEntries(names.map(name => [name, { type: 'reset' }]));
  Object.assign(value.graph, { lane_palette: [], author_palette: [] });
  return value;
}
const tick = () => new Promise(resolve => setImmediate(resolve));
function deferred() { let resolve; let reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }

function fixture() {
  const subscribers = new Set();
  const handlers = {};
  const errors = [];
  const picker = { items: [], activeItems: [], selectedItems: [],
    onDidChangeActive(fn) { handlers.active = fn; return { dispose() {} }; },
    onDidAccept(fn) { handlers.accept = fn; return { dispose() {} }; },
    onDidHide(fn) { handlers.hide = fn; return { dispose() {} }; },
    show() {}, hide() { handlers.hide?.(); }, dispose() {},
    choose(name) { const item = this.items.find(item => item.name === name); this.activeItems = [item]; this.selectedItems = [item]; handlers.active([item]); },
    accept() { handlers.accept(); } };
  const backend = { generation: 1, state: { status: 'ready' }, persisted: 'original',
    onDidChange(fn) { subscribers.add(fn); return { dispose() { subscribers.delete(fn); } }; },
    emit(state) { this.state = state; for (const fn of subscribers) fn(state); },
    theme: async function() { return theme(this.persisted); }, themesList: async function() { return { current: this.persisted, persisted: this.persisted, names: ['original', 'blue', 'red'] }; },
    themePreview: async name => ({ theme: theme(name), preview_token: 'a'.repeat(64) }),
    themeSave: async function(name) { this.persisted = name; return { theme: theme(name) }; } };
  const controller = new ThemeController(backend);
  const vscode = { window: { createQuickPick: () => picker, showErrorMessage: message => errors.push(message) } };
  const subject = new ThemePicker(vscode, backend, controller);
  return { backend, controller, subject, picker, errors, dispose() { subject.dispose(); controller.dispose(); } };
}

test('preview updates both subscribers, cancellation restores the exact previous theme', async () => {
  const f = fixture();
  try {
    await tick();
    const before = f.controller.value;
    const sidebar = []; const graph = [];
    f.controller.onDidChange(() => sidebar.push(f.controller.payload()?.name));
    f.controller.onDidChange(() => graph.push(f.controller.payload()?.name));
    const open = f.subject.open(); await tick();
    f.picker.choose('blue'); await tick();
    assert.equal(f.controller.value.name, 'blue');
    f.picker.hide(); await open;
    assert.equal(f.controller.value, before);
    assert.deepEqual(sidebar, ['blue', 'original']);
    assert.deepEqual(graph, sidebar);
  } finally { f.dispose(); }
});

test('accept saves the selected preview and save failures restore the original', async () => {
  const f = fixture();
  try {
    await tick();
    let request;
    f.backend.themeSave = async (...args) => { request = args; f.backend.persisted = args[0]; return { theme: theme(args[0]) }; };
    let open = f.subject.open(); await tick(); f.picker.choose('blue'); await tick(); f.picker.accept(); await open;
    assert.deepEqual(request.slice(0, 3), ['blue', 'original', 'a'.repeat(64)]);
    assert.equal(f.controller.value.name, 'blue');
    f.backend.themeSave = async () => { throw new Error('read-only config'); };
    open = f.subject.open(); await tick(); f.picker.choose('red'); await tick(); f.picker.accept(); await open;
    assert.equal(f.controller.value.name, 'blue');
    assert.match(f.errors[0], /read-only config/);
  } finally { f.dispose(); }
});

test('late previews and backend changes cannot overwrite newer theme state', async () => {
  const f = fixture();
  try {
    await tick(); const slow = deferred();
    f.backend.themePreview = name => name === 'blue' ? slow.promise : Promise.resolve({ theme: theme(name), preview_token: 'a'.repeat(64) });
    const open = f.subject.open(); await tick(); f.picker.choose('blue'); f.picker.choose('red'); await tick();
    assert.equal(f.controller.value.name, 'red');
    slow.resolve({ theme: theme('blue'), preview_token: 'a'.repeat(64) }); await tick();
    assert.equal(f.controller.value.name, 'red');
    f.backend.generation++; f.backend.emit({ status: 'initializing' }); await open;
    assert.equal(f.controller.value, undefined);
    f.backend.theme = async () => theme('replacement'); f.backend.emit({ status: 'ready' }); await tick();
    assert.equal(f.controller.value.name, 'replacement');
  } finally { f.dispose(); }
});

test('removed or invalid theme cannot be saved and the user can select another', async () => {
  const f = fixture();
  try {
    await tick(); let saved = false;
    f.backend.themePreview = async name => { if (name === 'blue') throw new Error('theme removed'); return { theme: theme(name), preview_token: 'a'.repeat(64) }; };
    f.backend.themeSave = async name => { saved = true; f.backend.persisted = name; return { theme: theme(name) }; };
    const open = f.subject.open(); await tick(); f.picker.choose('blue'); await tick(); f.picker.accept(); await tick();
    assert.equal(saved, false); assert.match(f.picker.validationMessage, /theme removed/);
    f.picker.choose('red'); await tick(); f.picker.accept(); await open;
    assert.equal(f.controller.value.name, 'red');
  } finally { f.dispose(); }
});

test('cancelling or disposing while a preview is pending does not apply the late result', async () => {
  const f = fixture();
  try {
    await tick();
    const slow = deferred();
    f.backend.themePreview = () => slow.promise;
    const before = f.controller.value;
    const open = f.subject.open(); await tick(); f.picker.choose('blue'); f.picker.hide(); await open;
    slow.resolve({ theme: theme('blue'), preview_token: 'a'.repeat(64) }); await tick();
    assert.equal(f.controller.value, before);
    const next = deferred(); f.backend.themePreview = () => next.promise;
    const reopened = f.subject.open(); await tick(); f.picker.choose('red'); f.subject.dispose(); await reopened;
    next.resolve({ theme: theme('red'), preview_token: 'a'.repeat(64) }); await tick();
    assert.equal(f.controller.value, before);
  } finally { f.dispose(); }
});

test('opening during initial theme loading waits for the existing request', async () => {
  const f = fixture();
  try {
    await tick();
    const pending = deferred();
    f.backend.generation++;
    f.backend.theme = () => pending.promise;
    f.backend.emit({ status: 'initializing' });
    f.backend.emit({ status: 'ready' });
    const open = f.subject.open();
    await tick(); assert.equal(f.errors.length, 0);
    pending.resolve(theme('original')); await tick();
    assert.ok(f.picker.items.some(item => item.name === 'blue'));
    f.picker.hide(); await open;
  } finally { f.dispose(); }
});

test('missing current theme can be previewed and replaced, or its error restored on cancel', async () => {
  const f = fixture();
  try {
    await tick(); f.backend.generation++;
    f.backend.theme = async () => { throw new Error('current theme is missing'); };
    f.backend.themesList = async () => ({ current: '../missing', persisted: '../missing', names: ['blue', 'red'] });
    f.backend.emit({ status: 'initializing' }); f.backend.emit({ status: 'ready' }); await tick();
    const originalError = f.controller.error;
    let open = f.subject.open(); await tick(); f.picker.choose('blue'); await tick();
    assert.equal(f.controller.value.name, 'blue');
    f.picker.hide(); await open;
    assert.equal(f.controller.value, undefined);
    assert.equal(f.controller.error, originalError);
    let saved;
    f.backend.themeSave = async (name, prior) => { saved = [name, prior]; f.backend.theme = async () => theme(name); return { theme: theme(name) }; };
    open = f.subject.open(); await tick(); f.picker.choose('red'); await tick(); f.picker.accept(); await open;
    assert.deepEqual(saved, ['red', '../missing']);
    assert.equal(f.controller.value.name, 'red');
    assert.equal(f.controller.error, undefined);
  } finally { f.dispose(); }
});

test('lost save reply keeps outcome unknown until worker exit and reloads the actual saved theme', async () => {
  const f = fixture();
  try {
    await tick(); const closed = deferred(); let persisted = 'original'; let reads = 0;
    f.backend.theme = async () => { reads++; return theme(persisted); };
    f.backend.themeSave = async name => {
      persisted = name;
      throw Object.assign(new Error('lost response'), { code: 'outcome_unknown', workerClosed: closed.promise });
    };
    const open = f.subject.open(); await tick(); f.picker.choose('blue'); await tick(); f.picker.accept(); await open;
    assert.equal(f.controller.value, undefined);
    assert.match(f.controller.error, /outcome is unknown/i);
    assert.match(f.errors[0], /lost response/);
    assert.equal(reads, 0, 'the worker may still be writing');
    closed.resolve(); await tick(); await tick();
    assert.equal(f.controller.value.name, 'blue');
    assert.equal(f.controller.error, undefined);
    assert.equal(reads, 1);
  } finally { f.dispose(); }
});

test('a failed reload after an uncertain save remains visible', async () => {
  const f = fixture();
  try {
    await tick();
    f.backend.themeSave = async () => { throw Object.assign(new Error('lost response'), { code: 'outcome_unknown' }); };
    f.backend.theme = async () => { throw new Error('config inaccessible'); };
    const open = f.subject.open(); await tick(); f.picker.choose('blue'); await tick(); f.picker.accept(); await open; await tick();
    assert.match(f.controller.error, /config inaccessible/);
    assert.ok(f.errors.some(message => /could not reload.*config inaccessible/i.test(message)));
  } finally { f.dispose(); }
});

test('a save finishing after workspace reconfiguration refreshes the new backend theme', async () => {
  const f = fixture();
  try {
    await tick(); const saved = deferred();
    f.backend.themeSave = () => saved.promise;
    const open = f.subject.open(); await tick(); f.picker.choose('blue'); await tick(); f.picker.accept(); await tick();
    f.backend.generation++; f.backend.emit({ status: 'initializing' });
    f.backend.emit({ status: 'ready' }); await tick();
    assert.equal(f.controller.value.name, 'original');
    f.backend.persisted = 'blue'; saved.resolve({ theme: theme('blue') });
    await open; await tick(); await tick();
    assert.equal(f.controller.value.name, 'blue');
  } finally { f.dispose(); }
});

test('reopening during an uncertain save preserves the new preview and updates its cancel baseline', async () => {
  const f = fixture();
  try {
    await tick(); const closed = deferred();
    f.backend.themeSave = async () => { throw Object.assign(new Error('lost response'), { code: 'outcome_unknown', workerClosed: closed.promise }); };
    let open = f.subject.open(); await tick(); f.picker.choose('blue'); await tick(); f.picker.accept(); await open;
    open = f.subject.open(); await tick(); f.picker.choose('red'); await tick();
    assert.equal(f.controller.value.name, 'red');
    f.backend.persisted = 'blue'; closed.resolve(); await tick(); await tick();
    assert.equal(f.controller.value.name, 'red', 'reconciliation must not replace the newer preview');
    f.picker.hide(); await open;
    assert.equal(f.controller.value.name, 'blue', 'cancel restores the theme actually saved by the earlier worker');
  } finally { f.dispose(); }
});
