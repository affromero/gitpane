'use strict';

const path = require('node:path');
const { relativeFile } = require('./change-pager');

// VS Code's metadata identifies candidates only. The backend verifies the
// registration, gitlink, repository identities, and workspace boundary.
class SubmoduleHierarchy {
  constructor(tree) {
    this.tree = tree;
    this.edges = new Map();
    this.generation = 0;
    this.error = undefined;
    this.timer = undefined;
  }

  schedule() {
    if (this.timer !== undefined || this.tree.disposed) return;
    this.timer = setTimeout(async () => {
      this.timer = undefined;
      try {
        await this.tree.submoduleDiscovery.refresh();
        await this.refresh();
        this.tree.scheduleChanged();
      } catch (error) {
        this.error = `Could not verify submodule hierarchy: ${error.message}`;
        this.tree.scheduleChanged();
      }
    }, 100);
    this.timer.unref?.();
  }

  async refresh() {
    const tree = this.tree;
    const generation = ++this.generation;
    if (typeof tree.backend.validateSubmodulePin !== 'function') return;
    const view = tree.workspaceView();
    const records = view.records;
    const edges = new Map();
    let error;
    for (const parent of records) {
      const paths = parent.repo.state?.submodules;
      const parentIdentity = parent.status?.repo_identity;
      if (!Array.isArray(paths) || !/^[a-f0-9]{64}$/.test(parentIdentity || '')) continue;
      for (const item of paths) {
        const relative = item?.path;
        if (!relativeFile(relative)) continue;
        const expected = tree.directoryIdentity(path.join(parent.identity.path, ...relative.split('/')));
        const child = records.find(record => expected && record.identity.path === expected.path &&
          record.identity.dev === expected.dev && record.identity.ino === expected.ino);
        if (!child || child.explicit || child === parent || !/^[a-f0-9]{64}$/.test(child.status?.repo_identity || '')) continue;
        const parentToken = tree.repositoryToken(parent.repo);
        const childToken = tree.repositoryToken(child.repo);
        const parentRevision = tree.contentRevision(parent.repo);
        const childRevision = tree.contentRevision(child.repo);
        const current = () => !tree.disposed &&
          tree.isCurrentRepository(parent.repo, parentToken) && tree.isCurrentRepository(child.repo, childToken) &&
          tree.backendStatuses.get(parent.key)?.repo_identity === parentIdentity &&
          tree.backendStatuses.get(child.key)?.repo_identity === child.status.repo_identity;
        const unchanged = () => current() && tree.contentRevision(parent.repo) === parentRevision &&
          tree.contentRevision(child.repo) === childRevision;
        try {
          const result = await tree.backend.validateSubmodulePin(parent.identity.path, [{ path: relative,
            parent_identity: parentIdentity, child_identity: child.status.repo_identity }]);
          if (generation !== this.generation || !unchanged()) return;
          const roots = result?.repos;
          const actualParent = Array.isArray(roots) && roots.length === 2 && tree.directoryIdentity(roots[0]);
          const actualChild = Array.isArray(roots) && roots.length === 2 && tree.directoryIdentity(roots[1]);
          if (actualParent?.path !== parent.identity.path || actualChild?.path !== child.identity.path ||
              actualParent.dev !== parent.identity.dev || actualParent.ino !== parent.identity.ino ||
              actualChild.dev !== child.identity.dev || actualChild.ino !== child.identity.ino) {
            throw new Error('Gitpane returned invalid submodule hierarchy details.');
          }
          if (!edges.has(child.key)) edges.set(child.key, { parent: parent.key, current });
        } catch (failure) {
          if (generation !== this.generation || !unchanged()) return;
          if (!['stale_target', 'outside_workspace', 'invalid_request'].includes(failure.code)) {
            error = `Could not verify submodule hierarchy: ${failure.message}`;
          }
        }
      }
    }
    if (generation !== this.generation || tree.disposed) return;
    this.edges = edges;
    this.error = error;
  }

  dispose() {
    this.generation++;
    if (this.timer !== undefined) clearTimeout(this.timer);
  }
}

module.exports = { SubmoduleHierarchy };
