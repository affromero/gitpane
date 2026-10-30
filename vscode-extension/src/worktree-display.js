'use strict';

class WorktreeDisplay {
  constructor(backend) {
    this.backend = backend;
    this.listeners = new Set();
    this.generation = undefined;
    this.epoch = 0;
    this.value = undefined;
    this.launcherKeys = [];
    this.reviewConfigured = false;
    this.error = undefined;
    this.disposed = false;
    this.subscription = backend.onDidChange(state => {
      if (state.livenessChanged) return;
      if (state.status === 'ready') { this.load(); return; }
      if (state.status !== 'initializing' && state.status !== 'error') return;
      this.epoch++;
      this.generation = undefined;
      this.value = undefined;
      this.launcherKeys = [];
      this.reviewConfigured = false;
      this.error = state.status === 'error' ? `Gitpane worktree display: ${state.error?.message || 'Backend unavailable.'}` : undefined;
      this.notify();
    });
    if (backend.state?.status === 'ready') this.load();
  }

  onDidChange(handler) { this.listeners.add(handler); return { dispose: () => this.listeners.delete(handler) }; }
  notify() { for (const listener of this.listeners) listener(); }

  async retry() {
    if (this.disposed) return;
    this.generation = undefined;
    await this.load();
  }

  async load() {
    const generation = this.backend.generation;
    if (this.disposed || this.generation === generation) return;
    this.generation = generation;
    const epoch = ++this.epoch;
    try {
      const config = await this.backend.uiConfig();
      if (!config || typeof config.expand_worktrees !== 'boolean') throw new Error('Gitpane returned invalid worktree display settings.');
      if ((config.launcher_keys !== undefined && (!Array.isArray(config.launcher_keys) ||
          !config.launcher_keys.every(key => typeof key === 'string' && [...key].length === 1 && !/[\x00-\x1f\x7f]/.test(key)))) ||
          (config.review_configured !== undefined && typeof config.review_configured !== 'boolean')) throw new Error('Gitpane returned invalid launcher settings.');
      if (this.disposed || this.backend.generation !== generation || this.epoch !== epoch) return;
      this.value = config.expand_worktrees;
      this.launcherKeys = config.launcher_keys || [];
      this.reviewConfigured = config.review_configured === true;
      this.error = undefined;
    } catch (error) {
      if (this.disposed || this.backend.generation !== generation || this.epoch !== epoch) return;
      this.value = undefined;
      this.launcherKeys = [];
      this.reviewConfigured = false;
      this.error = `Gitpane worktree display: ${error.message}`;
    }
    this.notify();
  }

  dispose() { this.disposed = true; this.epoch++; this.subscription.dispose(); this.listeners.clear(); }
}

module.exports = { WorktreeDisplay };
