'use strict';

async function receiveViewMessage(provider, vscode, message, surface = 'repositories') {
  if (provider.workspaceViews && !provider.workspaceViews.accepts(message, surface)) return;
  const guard = provider.workspaceViews?.messageGuard(message, surface) || (() => !provider.disposed);
  if (message?.type === 'ready') {
    provider.ready = true;
    provider.publish();
  } else if (message?.type === 'selectOwner' && typeof message.id === 'string') {
    provider.workspaceViews?.select(message.id);
  } else if (message?.type === 'ackWorktreeReveal' && typeof message.id === 'string') {
    provider.worktreeReveals.delete(message.id);
    if (provider.worktreeRevealError && provider.worktreeReveals.size < 128) {
      provider.worktreeRevealError = undefined;
      provider.publish();
    }
  } else if (message?.type === 'refresh') {
    try { await provider.refresh(true); }
    catch (error) {
      provider.tree.fail(`Could not refresh repository status: ${error.message}`);
      provider.tree.changed();
    }
  } else if (message?.type === 'openChange' && typeof message.id === 'string') {
    const selected = provider.changes.get(message.id);
    const node = selected && await provider.waitForCurrentChange(selected);
    if (!guard()) return;
    if (node) await vscode.commands.executeCommand('gitpane.openChange', node, ...(surface === 'changes' ? [guard] : []));
    else if (selected) await vscode.window.showErrorMessage('Gitpane change is no longer available. Refresh and try again.');
  } else if (message?.type === 'loadChanges' && typeof message.id === 'string') {
    provider.workspaceViews?.retainChanges(surface, message.id);
    await provider.loadChanges(message.id);
  } else if (message?.type === 'changesPage' && typeof message.id === 'string') {
    await provider.pager.move(message.id, message.direction);
  } else if (message?.type === 'searchChanges' && typeof message.id === 'string' &&
             typeof message.query === 'string' && ['first', 'next', 'previous'].includes(message.direction)) {
    const request = ++provider.nextSearchRequest;
    provider.searchRequests.set(message.id, request);
    if (!provider.currentOwner(message.id)) return;
    if (!provider.pager.get(message.id)) await provider.loadChanges(message.id);
    if (!guard() || provider.searchRequests.get(message.id) !== request || !provider.currentOwner(message.id)) return;
    await provider.pager.search(message.id, message.query, message.direction);
  } else if (message?.type === 'releaseChanges' && typeof message.id === 'string') {
    if (provider.workspaceViews) provider.workspaceViews.releaseChanges(surface, message.id);
    else {
      provider.searchRequests.delete(message.id);
      provider.pager.release(message.id);
    }
    provider.publish();
  } else if (message?.type === 'openGraph' && typeof message.id === 'string') {
    if (provider.currentRepository(message.id)) await vscode.commands.executeCommand('gitpane.openGraph', message.id);
  } else if (message?.type === 'openWorktreeGraph' && typeof message.id === 'string') {
    await provider.openWorktreeGraph?.(message.id);
  } else if (message?.type === 'shortcut' && typeof message.id === 'string' && typeof message.key === 'string') {
    await provider.handleShortcut(message, guard);
  } else if (message?.type === 'menu' && typeof message.id === 'string') {
    if (message.kind === 'file') {
      const selected = provider.changes.get(message.id);
      const current = selected && await provider.waitForCurrentChange(selected);
      if (!guard()) return;
      if (selected && !current) {
        await vscode.window.showErrorMessage('Gitpane change is no longer available. Refresh and try again.');
        return;
      }
    }
    if (guard()) await provider.actions?.showMenu(message.kind, message.id, guard);
  }
}

module.exports = { receiveViewMessage };
