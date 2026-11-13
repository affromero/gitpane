'use strict';

const { BridgeError } = require('./bridge-client');

// Comparisons run in their own service so a large tree diff cannot block the
// main bridge. A newer selection cancels the previous request.
class ComparisonService {
  constructor(backend) {
    this.backend = backend;
    this.generation = 0;
    this.client = undefined;
  }

  cancel() {
    this.generation++;
    this.client?.dispose({ immediate: true });
    this.client = undefined;
  }

  async request(method, repo, params, guard = () => true) {
    if (!['resolve_revision', 'compare_revisions', 'revision_refs'].includes(method)) {
      throw new BridgeError('invalid_request', 'Unsupported revision comparison request.');
    }
    this.cancel();
    const generation = this.generation;
    const snapshot = await this.backend.mutationSnapshot();
    const external = this.backend.externalSelections.get(repo);
    const current = () => generation === this.generation && guard() &&
      this.backend.vscode.workspace.isTrusted === true && snapshot.generation === this.backend.generation &&
      (external ? this.backend.externalSelections.get(repo) === external && external.guard() &&
        this.backend.isCurrentMutation(snapshot, external.selection.repo) :
        this.backend.isCurrentMutation(snapshot, repo));
    if (!current()) throw new BridgeError('stale_target', 'Repository is no longer available.');
    const client = this.backend.createClient(snapshot.executable, {
      onStderr: chunk => this.backend.output?.append(chunk), timeoutMs: 30000
    });
    this.client = client;
    try {
      await client.initialize(snapshot.roots, snapshot.rootIdentities, snapshot.rootSources);
      if (!current()) throw new BridgeError('stale_target', 'Repository is no longer available.');
      const request = { repo, ...params };
      const result = external ? await client.request('external_worktree_read', {
        selection: external.selection, method, params: request
      }) : await client.request(method, request);
      if (!current()) throw new BridgeError('stale_target', 'Repository is no longer available.');
      return result;
    } catch (error) {
      if (error?.code === 'invalid_request' && /unknown bridge method|unsupported external worktree operation/i.test(error.message)) {
        throw new BridgeError('unsupported_backend', 'The configured Gitpane backend does not support revision comparison. Update the backend and try again.');
      }
      throw error;
    } finally {
      if (this.client === client) this.client = undefined;
      await client.shutdown({ immediate: true });
    }
  }

  dispose() { this.cancel(); }
}

module.exports = { ComparisonService };
