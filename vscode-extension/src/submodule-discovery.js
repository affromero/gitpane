'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { relativeFile } = require('./change-pager');

class SubmoduleDiscovery {
  constructor(tree, vscode) {
    this.tree = tree;
    this.vscode = vscode;
    this.enabled = true;
    this.generation = 0;
    this.openings = new Map();
    this.error = undefined;
  }

  configure(enabled) {
    if (this.enabled === enabled) return false;
    this.enabled = enabled;
    this.generation++;
    this.error = undefined;
    return true;
  }

  async refresh() {
    const tree = this.tree;
    if (!this.enabled || typeof tree.git.openRepository !== 'function' ||
        typeof tree.backend.discoverSubmodule !== 'function' || typeof tree.backend.validateSubmodulePin !== 'function') return;
    const generation = this.generation;
    const backendGeneration = tree.backendGeneration;
    const records = tree.presentationView().records;
    const seen = new Set(records.map(record => record.identity.path));
    let error;
    for (const parent of records) {
      const parentIdentity = parent.status?.repo_identity;
      if (!/^[a-f0-9]{64}$/.test(parentIdentity || '') || !Array.isArray(parent.repo.state?.submodules)) continue;
      const token = tree.repositoryToken(parent.repo);
      const revision = tree.contentRevision(parent.repo);
      for (const entry of parent.repo.state.submodules) {
        const relative = entry?.path;
        if (!relativeFile(relative) || relative.length > 4096) continue;
        const target = path.join(parent.identity.path, ...relative.split('/'));
        const identity = tree.directoryIdentity(target);
        if (!identity || !tree.isPathInWorkspace(identity.path) || seen.has(identity.path)) continue;
        try {
          const dotGit = fs.lstatSync(path.join(identity.path, '.git'));
          if (!dotGit.isFile() && !dotGit.isDirectory()) continue;
        } catch { continue; }
        const current = () => this.enabled && generation === this.generation && backendGeneration === tree.backendGeneration &&
          tree.isCurrentRepository(parent.repo, token) && tree.contentRevision(parent.repo) === revision &&
          tree.backendStatuses.get(parent.key)?.repo_identity === parentIdentity &&
          tree.presentationView().records.some(record => record.key === parent.key) &&
          tree.isPathInWorkspace(identity.path) && (() => {
            const now = tree.directoryIdentity(target);
            return now && now.path === identity.path && now.dev === identity.dev && now.ino === identity.ino;
          })();
        if (!current() || this.openings.has(identity.path)) continue;
        try {
          const details = await tree.backend.discoverSubmodule(parent.identity.path, relative, parentIdentity);
          if (!current()) return;
          if (!details || details.parent_identity !== parentIdentity || !/^[a-f0-9]{64}$/.test(details.child_identity || '') ||
              typeof details.repo !== 'string' || !path.isAbsolute(details.repo) || tree.directoryIdentity(details.repo)?.path !== identity.path) {
            throw new Error('Gitpane returned invalid submodule discovery details.');
          }
          const allowed = () => current() && !tree.hiddenRepositories?.isHidden(identity, details.child_identity);
          if (!allowed()) continue;
          const proof = await tree.backend.validateSubmodulePin(parent.identity.path, [{ path: relative,
            parent_identity: parentIdentity, child_identity: details.child_identity }]);
          if (!allowed()) continue;
          if (!Array.isArray(proof?.repos) || proof.repos.length !== 2 ||
              tree.directoryIdentity(proof.repos[0])?.path !== parent.identity.path ||
              tree.directoryIdentity(proof.repos[1])?.path !== identity.path) throw new Error('Gitpane returned invalid submodule validation details.');
          seen.add(identity.path);
          if (this.openings.has(identity.path) || tree.git.repositories.some(repo => tree.directoryIdentity(repo.rootUri.fsPath)?.path === identity.path)) continue;
          const opening = Promise.resolve().then(() => allowed() ? tree.git.openRepository(this.vscode.Uri.file(identity.path)) : undefined);
          this.openings.set(identity.path, opening);
          let opened;
          try { opened = await opening; } finally { this.openings.delete(identity.path); }
          if (!current()) return;
          if (opened && tree.directoryIdentity(opened.rootUri.fsPath)?.path === identity.path &&
              tree.git.repositories.some(repo => repo.rootUri.toString() === opened.rootUri.toString())) tree.observe(opened);
        } catch (failure) {
          if (!current()) return;
          if (!['stale_target', 'outside_workspace', 'invalid_request'].includes(failure.code) &&
              !(failure.code === 'repository_error' && /not initialized or unavailable/i.test(failure.message))) {
            error = `Could not discover submodule ${target}: ${failure.message}`;
          }
        }
      }
    }
    if (generation === this.generation && !tree.disposed) this.error = error;
  }
}

module.exports = { SubmoduleDiscovery };
