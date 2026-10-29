'use strict';

const ANSI = ['black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'gray',
  'dark_gray', 'light_red', 'light_green', 'light_yellow', 'light_blue', 'light_magenta', 'light_cyan', 'white'];
const VS_ANSI = ['Black', 'Red', 'Green', 'Yellow', 'Blue', 'Magenta', 'Cyan', 'White',
  'BrightBlack', 'BrightRed', 'BrightGreen', 'BrightYellow', 'BrightBlue', 'BrightMagenta', 'BrightCyan', 'BrightWhite'];
const byte = value => Number.isInteger(value) && value >= 0 && value <= 255;
const keys = (value, expected) => value && typeof value === 'object' && !Array.isArray(value) &&
  Object.keys(value).sort().join(',') === [...expected].sort().join(',');

function color(value) {
  if (keys(value, ['type']) && value.type === 'reset') return 'currentColor';
  if (keys(value, ['type', 'name']) && value.type === 'ansi' && ANSI.includes(value.name)) {
    return `var(--vscode-terminal-ansi${VS_ANSI[ANSI.indexOf(value.name)]})`;
  }
  if (keys(value, ['type', 'value']) && value.type === 'indexed' && byte(value.value)) {
    const index = value.value;
    if (index < 16) return `var(--vscode-terminal-ansi${VS_ANSI[index]})`;
    if (index >= 232) return `rgb(${[8 + (index - 232) * 10, 8 + (index - 232) * 10, 8 + (index - 232) * 10].join(', ')})`;
    const digits = [Math.floor((index - 16) / 36), Math.floor((index - 16) / 6) % 6, (index - 16) % 6];
    return `rgb(${digits.map(digit => digit === 0 ? 0 : 55 + digit * 40).join(', ')})`;
  }
  if (keys(value, ['type', 'red', 'green', 'blue']) && value.type === 'rgb' &&
      [value.red, value.green, value.blue].every(byte)) return `rgb(${value.red}, ${value.green}, ${value.blue})`;
  throw new Error('Gitpane returned an invalid theme color.');
}

const FIELDS = {
  repo_list: ['dirty_marker', 'branch', 'branch_default', 'ahead', 'behind', 'worktree_count',
    'dirty_submodule', 'unpushed_submodule', 'stash', 'file_count', 'repo_name', 'worktree_subtree_icon', 'worktree_subtree_branch', 'live'],
  file_list: ['status_modified', 'status_added', 'status_deleted', 'status_renamed', 'status_untracked',
    'status_conflicted', 'submodule_path', 'regular_path', 'submodule_bracket', 'submodule_branch',
    'submodule_unpushed', 'submodule_unreachable', 'submodule_needs_merge'],
  graph: ['commit_id', 'commit_message', 'merge_message', 'time', 'addition', 'deletion', 'paren',
    'head_marker', 'worktree_marker', 'tag_label', 'remote_label', 'local_branch_label', 'stash_label',
    'dimmed', 'commit_files_status_modified', 'commit_files_status_added', 'commit_files_status_deleted',
    'commit_files_status_renamed', 'commit_files_status_other', 'commit_files_path']
};

function validateTheme(theme) {
  if (!theme || typeof theme.name !== 'string' || !theme.name || theme.name.length > 256) throw new Error('Gitpane returned an invalid theme.');
  const values = {};
  for (const [section, names] of Object.entries(FIELDS)) {
    const item = theme[section];
    if (!item || typeof item !== 'object' || Array.isArray(item)) throw new Error('Gitpane returned an invalid theme.');
    for (const name of names) values[`${section.replaceAll('_', '-')}-${name.replaceAll('_', '-')}`] = color(item[name]);
  }
  const palette = key => {
    if (!Array.isArray(theme.graph[key]) || theme.graph[key].length > 4096) throw new Error('Gitpane returned an invalid theme palette.');
    return theme.graph[key].map(color);
  };
  return { name: theme.name, values, lanes: palette('lane_palette'), authors: palette('author_palette') };
}

class ThemeController {
  constructor(backend, onColorThemeChange) {
    this.backend = backend;
    this.listeners = new Set();
    this.generation = Symbol('unloaded');
    this.value = undefined;
    this.error = undefined;
    this.disposed = false;
    this.epoch = 0;
    this.requestEpoch = undefined;
    this.appearance = 0;
    this.appearanceSubscription = onColorThemeChange?.(() => { this.appearance++; this.notify(); });
    this.subscription = backend.onDidChange(state => {
      if (state.status === 'ready') { this.load(); return; }
      if (state.status !== 'initializing' && state.status !== 'error') return;
      const wasRequesting = this.requestEpoch !== undefined;
      this.requestEpoch = undefined;
      this.epoch++;
      this.value = undefined;
      this.error = state.status === 'error' ? `Gitpane theme: ${state.error?.message || 'Backend unavailable.'}` : undefined;
      if (state.status === 'initializing' || !wasRequesting) this.generation = Symbol('unloaded');
      this.notify();
    });
    if (backend.state?.status === 'ready') this.load();
  }
  onDidChange(handler) { this.listeners.add(handler); return { dispose: () => this.listeners.delete(handler) }; }
  payload() { return this.value && { ...this.value, appearance: this.appearance }; }
  notify() { for (const listener of this.listeners) listener(); }
  async retry() {
    if (this.disposed) return;
    if (this.pendingLoad && this.pendingGeneration === this.backend.generation) return this.pendingLoad;
    this.generation = Symbol('retry');
    await this.load();
  }
  beginPreview() {
    this.epoch++;
    this.requestEpoch = undefined;
    return { value: this.value, error: this.error, generation: this.backend.generation, epoch: this.epoch };
  }
  previewCurrent(session) {
    return !this.disposed && this.backend.generation === session.generation && this.epoch === session.epoch;
  }
  applyPreview(session, value, error) {
    if (!this.previewCurrent(session)) return false;
    this.value = value;
    this.error = error;
    this.notify();
    return true;
  }
  load() {
    const generation = this.backend.generation;
    if (this.pendingLoad && this.pendingGeneration === generation) return this.pendingLoad;
    const pending = this.loadTheme();
    this.pendingLoad = pending;
    this.pendingGeneration = generation;
    pending.finally(() => { if (this.pendingLoad === pending) this.pendingLoad = undefined; });
    return pending;
  }
  async loadTheme() {
    const generation = this.backend.generation;
    if (this.disposed || this.generation === generation) return;
    this.generation = generation;
    const epoch = ++this.epoch;
    this.requestEpoch = epoch;
    try {
      const theme = validateTheme(await this.backend.theme());
      if (this.disposed || this.generation !== generation || this.backend.generation !== generation || this.epoch !== epoch) return;
      this.value = theme;
      this.error = undefined;
    } catch (error) {
      if (this.disposed || this.generation !== generation || this.backend.generation !== generation || this.epoch !== epoch) return;
      this.value = undefined;
      this.error = `Gitpane theme: ${error.message}`;
    } finally { if (this.requestEpoch === epoch) this.requestEpoch = undefined; }
    this.notify();
  }
  dispose() { this.disposed = true; this.subscription.dispose(); this.appearanceSubscription?.dispose(); this.listeners.clear(); }
}

module.exports = { color, validateTheme, ThemeController };
