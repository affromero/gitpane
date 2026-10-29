'use strict';

class SessionLiveness {
  constructor(tree, backend) {
    this.tree = tree;
    this.backend = backend;
    this.visible = false;
    this.disposed = false;
    this.epoch = 0;
    this.timer = undefined;
    this.pending = undefined;
    this.signature = undefined;
    this.live = new Map();
    this.error = undefined;
  }

  setVisible(visible) {
    if (this.visible === visible || this.disposed) return;
    this.visible = visible;
    this.epoch++;
    if (this.timer !== undefined) clearTimeout(this.timer);
    this.timer = undefined;
    if (!visible) {
      this.backend.cancelSessionLiveness();
      this.live.clear();
      this.error = undefined;
      this.signature = undefined;
      this.lastProbe = undefined;
      return;
    }
    this.signature = JSON.stringify(this.tree.sessionTargets());
    this.schedule(0);
  }

  changed() {
    if (!this.visible || this.disposed) return;
    const signature = JSON.stringify(this.tree.sessionTargets());
    if (signature === this.signature) return;
    this.signature = signature;
    this.epoch++;
    this.live.clear();
    this.error = undefined;
    if (this.pending) this.backend.cancelSessionLiveness();
    if (this.timer !== undefined) clearTimeout(this.timer);
    this.timer = undefined;
    this.schedule(0);
  }

  restart() {
    if (!this.visible || this.disposed) return;
    this.epoch++;
    this.backend.cancelSessionLiveness();
    this.live.clear();
    this.error = undefined;
    this.lastProbe = undefined;
    if (this.timer !== undefined) clearTimeout(this.timer);
    this.timer = undefined;
    this.schedule(0);
  }

  schedule(delay) {
    if (!this.visible || this.disposed || this.pending || this.timer !== undefined) return;
    this.timer = setTimeout(() => {
      this.timer = undefined;
      const pending = this.probe();
      this.pending = pending;
      pending.finally(() => {
        if (this.pending !== pending) return;
        this.pending = undefined;
        this.schedule(this.signature === this.lastProbe ? 5000 : 0);
      });
    }, delay);
  }

  async probe() {
    const targets = this.tree.sessionTargets();
    const signature = JSON.stringify(targets);
    const epoch = this.epoch;
    const generation = this.backend.generation;
    this.lastProbe = signature;
    if (!targets.length) { this.live.clear(); this.error = undefined; return; }
    try {
      const paths = targets.map(target => target.path);
      const result = await this.backend.sessionLiveness(paths);
      if (!this.visible || this.disposed || epoch !== this.epoch || generation !== this.backend.generation ||
          signature !== JSON.stringify(this.tree.sessionTargets())) return;
      if (!result || typeof result.enabled !== 'boolean' || !Array.isArray(result.repos) ||
          (result.enabled ? result.reason !== null || result.repos.length !== paths.length :
            !['disabled', 'unsupported'].includes(result.reason) || result.repos.length !== 0) ||
          result.repos.some((entry, index) => !entry || entry.repo !== paths[index] || typeof entry.live !== 'boolean')) {
        throw new Error('Gitpane returned an invalid session liveness result.');
      }
      this.live = new Map(result.repos.map(entry => [entry.repo, entry.live]));
      this.error = undefined;
    } catch (error) {
      if (!this.visible || this.disposed || epoch !== this.epoch || generation !== this.backend.generation ||
          signature !== JSON.stringify(this.tree.sessionTargets())) return;
      this.live.clear();
      this.error = `Gitpane sessions: ${error?.message || String(error)}`;
    }
    this.tree.changed();
  }

  dispose() {
    if (this.disposed) return;
    this.setVisible(false);
    this.disposed = true;
  }
}

module.exports = { SessionLiveness };
