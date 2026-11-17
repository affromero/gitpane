'use strict';

const { BridgeError } = require('./bridge-client');

async function openChangesSession(manager, repo) {
  const snapshot = await manager.mutationSnapshot();
  const external = manager.externalSelections.get(repo);
  const repoPath = await manager.realpath(repo);
  const anchor = external ? await manager.realpath(external.selection.repo) : repoPath;
  const valid = () => manager.isCurrentMutation(snapshot, anchor) &&
    (!external || (manager.externalSelections.get(repo) === external && external.guard()));
  if (!valid()) throw new BridgeError('workspace_changed', 'Workspace changed before checking the file.');
  if (manager.verifyClients.size >= 4) throw new BridgeError('busy', 'Too many file checks are running. Try again.');
  const client = manager.createClient(snapshot.executable, { onStderr: chunk => manager.output?.append(chunk) });
  manager.verifyClients.add(client);
  let closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    manager.verifyClients.delete(client);
    client.dispose({ immediate: true });
  };
  const current = async () => {
    if (closed || !valid()) return false;
    const resolved = await manager.realpath(repo);
    const currentAnchor = external ? await manager.realpath(external.selection.repo) : resolved;
    return !closed && valid() && resolved === repoPath && currentAnchor === anchor;
  };
  const run = async operation => {
    if (!await current()) throw new BridgeError('workspace_changed', 'Workspace changed before checking the file.');
    const result = await operation();
    if (!await current()) throw new BridgeError('workspace_changed', 'Workspace changed while checking the file.');
    return result;
  };
  try {
    await client.initialize(snapshot.roots, snapshot.rootIdentities, snapshot.rootSources);
    if (!await current()) throw new BridgeError('workspace_changed', 'Workspace changed before checking the file.');
    const externalRead = (method, params) => client.request('external_worktree_read',
      { selection: external.selection, method, params: { repo, ...params } });
    return {
      begin: () => run(() => external ? externalRead('changes_begin', {}) : client.beginChanges(repoPath)),
      next: (id, cursor) => run(() => external ? externalRead('changes_next', { snapshot: id, cursor }) : client.nextChanges(repoPath, id, cursor)),
      find: (id, record) => run(() => external ? externalRead('changes_find', { snapshot: id, path: record.path, group: record.group }) :
        client.findChange(repoPath, id, record.path, record.group)),
      close
    };
  } catch (error) { close(); throw error; }
}

module.exports = { openChangesSession };
