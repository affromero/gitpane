'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');
const { BridgeClient } = require('../../src/bridge-client');
const { MutationManager } = require('../../src/mutation-manager');
const { BackendManager } = require('../../src/backend-manager');
const { SessionLiveness } = require('../../src/session-liveness');
const { isolatedGitEnv } = require('./git-env');

const executable = process.env.GITPANE_TEST_BINARY;
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-bridge-test-'));
const workspace = path.join(temporary, 'workspace with spaces');
const repo = path.join(workspace, 'repo');
const outside = path.join(temporary, 'outside');
const hooks = path.join(temporary, 'empty-hooks');
const gitEnv = isolatedGitEnv();

function git(directory, ...args) {
  const result = spawnSync('git', ['-C', directory, '-c', 'user.name=Gitpane Test',
    '-c', 'user.email=gitpane-test@example.invalid', '-c', `core.hooksPath=${hooks}`, ...args], {
    env: gitEnv, encoding: 'utf8'
  });
  if (result.error || result.status !== 0) {
    throw new Error(`git ${args[0]} failed: ${result.error?.message || result.stderr}`);
  }
  return result.stdout.trim();
}

function write(name, value) { fs.writeFileSync(path.join(repo, name), value); }

async function waitForFile(file) {
  const deadline = Date.now() + 10000;
  while (!fs.existsSync(file)) {
    if (Date.now() >= deadline) throw new Error(`Timed out waiting for ${file}`);
    await new Promise(resolve => setTimeout(resolve, 20));
  }
}

(async () => {
  let client, child, closed;
  let configuredClient, configuredChild, configuredClosed;
  let sessionClient, sessionChild, sessionClosed;
  let mutations;
  const workers = [];
  const release = path.join(temporary, 'release-git');
  try {
    if (!executable || !path.isAbsolute(executable) || !fs.existsSync(executable)) {
      throw new Error('Set GITPANE_TEST_BINARY to the absolute path of the built Gitpane service.');
    }
    for (const directory of [repo, outside, hooks]) fs.mkdirSync(directory, { recursive: true });
    gitEnv.GITPANE_CONFIG = path.join(temporary, 'default-config.toml');
    git(repo, 'init', '-q');
    git(outside, 'init', '-q');
    write('tracked.txt', 'initial\n');
    git(repo, 'add', '--', 'tracked.txt');
    git(repo, 'commit', '-qm', 'root commit');
    const rootOid = git(repo, 'rev-parse', 'HEAD');
    const base = git(repo, 'symbolic-ref', '--short', 'HEAD');
    git(repo, 'checkout', '-qb', 'topic');
    write('topic.txt', 'topic\n');
    git(repo, 'add', '--', 'topic.txt');
    git(repo, 'commit', '-qm', 'topic commit');
    git(repo, 'checkout', '-q', base);
    write('main.txt', 'main\n');
    git(repo, 'add', '--', 'main.txt');
    git(repo, 'commit', '-qm', 'main commit');
    git(repo, 'merge', '--no-ff', '-qm', 'merge topic', 'topic');
    const mergeOid = git(repo, 'rev-parse', 'HEAD');
    write('tracked.txt', 'staged\n');
    git(repo, 'add', '--', 'tracked.txt');
    write('tracked.txt', 'unstaged\n');
    write('new file.txt', 'untracked\n');

    const stderr = [];
    client = new BridgeClient(executable, {
      onStderr: chunk => stderr.push(chunk),
      spawn: (program, args, options) => {
        child = spawn(program, args, { ...options, env: gitEnv });
        closed = new Promise(resolve => child.once('close', (code, signal) => resolve({ code, signal })));
        return child;
      }
    });
    const root = fs.realpathSync.native(workspace);
    const repoPath = fs.realpathSync.native(repo);
    const outsidePath = fs.realpathSync.native(outside);
    assert.deepEqual(await client.initialize([root]), { version: 1 });

    if (process.platform !== 'win32') {
      const alias = path.join(temporary, 'workspace-alias');
      fs.symlinkSync(root, alias);
      const probe = new BridgeClient(executable, { spawn: (program, args, options) => spawn(program, args, { ...options, env: gitEnv }) });
      try {
        await probe.initialize([root], undefined, [alias]);
        const identity = (await probe.workspaceRootIdentities()).roots[0].identity;
        fs.unlinkSync(alias);
        fs.symlinkSync(outsidePath, alias);
        await assert.rejects(probe.initialize([root], [identity], [alias]), error => error.code === 'stale_target');
      } finally { probe.dispose({ immediate: true }); }
      console.log('PASS workspace source retarget is rejected during worker initialization');
    }

    const status = await client.status(repoPath);
    assert.equal(status.is_dirty, true);
    const tracked = status.files.find(file => file.path === 'tracked.txt');
    assert.ok(tracked && tracked.staged && tracked.unstaged, 'staged and unstaged target must be retained');
    assert.ok(status.files.some(file => file.path === 'new file.txt' && file.status === 'untracked'));
    console.log('PASS handshake and real repository status');

    const many = [];
    for (let index = 0; index < 75; index++) {
      const directory = path.join(workspace, `scale-${String(index).padStart(3, '0')}`);
      fs.mkdirSync(directory);
      git(directory, 'init', '-q');
      fs.writeFileSync(path.join(directory, 'change.txt'), 'change\n');
      many.push({ path: fs.realpathSync.native(directory) });
    }
    const openedAt = process.hrtime.bigint();
    for (const entry of many) {
      const response = await client.beginChanges(entry.path);
      assert.equal(response.changes[0].path, 'change.txt');
      entry.snapshot = response.snapshot;
    }
    const openMs = Number(process.hrtime.bigint() - openedAt) / 1e6;
    for (const index of [0, 37, 74]) {
      const entry = many[index];
      assert.equal((await client.nextChanges(entry.path, entry.snapshot, 0)).changes[0].path, 'change.txt');
    }
    const old = many[0];
    const replacement = await client.beginChanges(old.path);
    assert.notEqual(replacement.snapshot, old.snapshot);
    await assert.rejects(client.nextChanges(old.path, old.snapshot, 0), error => error.code === 'snapshot_stale');
    await client.releaseChanges(old.path, replacement.snapshot);
    assert.equal((await client.nextChanges(many[74].path, many[74].snapshot, 0)).changes[0].path, 'change.txt');
    console.log(`PASS 75 simultaneously open real snapshots (one workspace root, ${openMs.toFixed(0)} ms)`);

    const graph = await client.graph(repoPath, false);
    const merge = graph.rows.find(row => row.oid === mergeOid);
    assert.ok(merge && merge.is_merge && merge.parent_oids.length === 2, 'merge commit and parents must appear');
    assert.ok(graph.rows.some(row => row.oid === rootOid), 'root commit must appear');
    assert.ok(merge.lanes.length > 0, 'graph lanes must be provided');
    const firstParent = await client.graph(repoPath, true);
    assert.ok(firstParent.rows.some(row => row.oid === mergeOid), 'first-parent graph must include the merge');
    console.log('PASS graph and first-parent request');

    const initialGraphRevision = (await client.graphRevision(repoPath)).revision;
    assert.match(initialGraphRevision, /^[0-9a-f]{64}$/);
    assert.equal((await client.graphRevision(repoPath)).revision, initialGraphRevision);
    const topicOid = git(repo, 'rev-parse', 'refs/heads/topic');
    git(repo, 'update-ref', 'refs/heads/topic', rootOid);
    assert.equal(git(repo, 'rev-parse', 'HEAD'), mergeOid);
    assert.notEqual((await client.graphRevision(repoPath)).revision, initialGraphRevision);
    const updatedGraph = await client.graph(repoPath);
    assert.ok(updatedGraph.rows.find(row => row.oid === rootOid).labels.some(label => label.name === 'topic'));
    git(repo, 'update-ref', 'refs/heads/topic', topicOid);
    assert.equal((await client.graphRevision(repoPath)).revision, initialGraphRevision);
    console.log('PASS graph revision detects off-HEAD branch updates');

    git(repo, 'update-ref', 'refs/remotes/origin/config-check', rootOid);
    const configPath = path.join(temporary, 'graph-config.toml');
    const configuredWorktrees = path.join(root, 'managed-worktrees');
    fs.mkdirSync(configuredWorktrees);
    fs.writeFileSync(configPath, '[graph]\nbranches = "remote"\nlabel_max_len = 5\nshow_stats = false\n' +
      '\n[worktree]\ndir = "../managed-worktrees"\n');
    configuredClient = new BridgeClient(executable, {
      spawn: (program, args, options) => {
        configuredChild = spawn(program, args, { ...options, env: { ...gitEnv, GITPANE_CONFIG: configPath } });
        configuredClosed = new Promise(resolve => configuredChild.once('close', (code, signal) => resolve({ code, signal })));
        return configuredChild;
      }
    });
    await configuredClient.initialize([root]);
    const configuredGraph = await configuredClient.graph(repoPath);
    assert.equal(configuredGraph.label_max_len, 5);
    assert.ok(configuredGraph.branches.includes('origin/config-check'));
    assert.ok(!configuredGraph.branches.includes('topic'));
    assert.ok(configuredGraph.rows.length > 0);
    assert.ok(configuredGraph.rows.every(row => row.diff_stat === null));
    const configuredRevision = (await configuredClient.graphRevision(repoPath)).revision;
    git(repo, 'update-ref', 'refs/heads/topic', rootOid);
    assert.equal((await configuredClient.graphRevision(repoPath)).revision, configuredRevision);
    git(repo, 'update-ref', 'refs/heads/topic', topicOid);
    git(repo, 'update-ref', 'refs/remotes/origin/config-check', mergeOid);
    assert.notEqual((await configuredClient.graphRevision(repoPath)).revision, configuredRevision);
    git(repo, 'update-ref', 'refs/remotes/origin/config-check', rootOid);
    console.log('PASS graph configuration from the workspace-host config file');

    const graphConfig = fs.readFileSync(configPath, 'utf8');
    git(repo, 'symbolic-ref', 'refs/remotes/origin/HEAD', 'refs/remotes/origin/config-check');
    fs.appendFileSync(configPath, '\n[review]\ncommand = "reviewer {base} {path}"\nplacement = "command"\n');
    const reviewAction = (await configuredClient.listLaunchers(repoPath)).actions.find(action => action.id === 'review');
    const reviewPlan = await configuredClient.planLauncher(repoPath, reviewAction.id, reviewAction.fingerprint);
    assert.equal(reviewPlan.base_ref, 'origin/config-check');
    assert.deepEqual(reviewPlan.argv, ['reviewer', 'origin/config-check', repoPath]);
    assert.equal((await configuredClient.validateLauncher(repoPath, reviewAction.id, reviewAction.fingerprint, reviewPlan.base_ref)).valid, true);
    git(repo, 'symbolic-ref', 'refs/remotes/origin/HEAD', 'refs/remotes/origin/main');
    git(repo, 'update-ref', 'refs/remotes/origin/main', rootOid);
    await assert.rejects(configuredClient.validateLauncher(repoPath, reviewAction.id, reviewAction.fingerprint, reviewPlan.base_ref),
      error => error.code === 'stale_launcher');
    await assert.rejects(configuredClient.launcherChoices(repoPath, reviewAction.id, reviewAction.fingerprint, 'tmux', reviewPlan.base_ref),
      error => error.code === 'stale_launcher');
    fs.writeFileSync(configPath, graphConfig);
    console.log('PASS configured review bases are pinned across pending launcher choices');

    const plannedWorktree = await configuredClient.planWorktree(repoPath, 'feature/verify');
    const plannedParent = fs.statSync(path.dirname(plannedWorktree.path), { bigint: true });
    const configuredParent = fs.statSync(configuredWorktrees, { bigint: true });
    assert.deepEqual([plannedParent.dev, plannedParent.ino], [configuredParent.dev, configuredParent.ino]);
    assert.equal(path.basename(plannedWorktree.path), 'repo-feature-verify');
    assert.equal(plannedWorktree.problem, null);
    await configuredClient.initialize([repoPath]);
    const outsidePlan = await configuredClient.planWorktree(repoPath, 'feature/verify');
    assert.equal(outsidePlan.path, plannedWorktree.path);
    assert.match(outsidePlan.problem, /outside the workspace/i);
    await configuredClient.initialize([root]);

    const rootFiles = await client.commitFiles(repoPath, rootOid);
    assert.equal(rootFiles.message, 'root commit');
    assert.ok(rootFiles.files.some(file => file.path === 'tracked.txt' && file.status === 'A' && !file.is_submodule));
    const mergeFiles = await client.commitFiles(repoPath, mergeOid);
    assert.equal(mergeFiles.message, 'merge topic');
    assert.ok(mergeFiles.files.some(file => file.path === 'topic.txt' && file.status === 'A' && !file.is_submodule));
    console.log('PASS root and merge commit files');

    for (const request of [() => client.status(outsidePath), () => client.graph(outsidePath),
      () => client.graphRevision(outsidePath),
      () => client.commitFiles(outsidePath, rootOid)]) {
      await assert.rejects(request(), error => error.code === 'outside_workspace');
    }
    await client.initialize([]);
    await assert.rejects(client.status(repoPath), error => error.code === 'outside_workspace');
    await client.initialize([root]);
    assert.equal((await client.status(repoPath)).is_dirty, true);
    console.log('PASS workspace exclusion and reinitialization');

    let generation = 1;
    let holdGit = false;
    let useFakeSessions = false;
    const wrapperDir = path.join(temporary, 'git-wrapper');
    const sessionDir = path.join(temporary, 'session-wrapper');
    const gitStarted = path.join(temporary, 'git-started');
    if (process.platform !== 'win32') {
      fs.mkdirSync(wrapperDir);
      const gitPath = spawnSync('which', ['git'], { env: gitEnv, encoding: 'utf8' });
      if (gitPath.status !== 0) throw new Error('Cannot locate Git for delayed mutation test.');
      const wrapper = path.join(wrapperDir, 'git');
      fs.writeFileSync(wrapper, '#!/bin/sh\n: > "$GITPANE_TEST_GIT_STARTED"\nwhile [ ! -e "$GITPANE_TEST_GIT_RELEASE" ]; do /bin/sleep 0.02; done\nexec "$GITPANE_TEST_REAL_GIT" "$@"\n', { mode: 0o755 });
      gitEnv.GITPANE_TEST_REAL_GIT = gitPath.stdout.trim();
      gitEnv.GITPANE_TEST_GIT_STARTED = gitStarted;
      gitEnv.GITPANE_TEST_GIT_RELEASE = release;
    }
    const rootIdentities = (await client.workspaceRootIdentities()).roots.map(entry => entry.identity);
    mutations = new MutationManager({
      getSnapshot: () => ({ executable, roots: [root], rootSources: [root], rootIdentities, generation }),
      isCurrent: snapshot => snapshot.generation === generation,
      spawn: (program, args, options) => {
        const env = { ...gitEnv };
        if (holdGit) env.PATH = `${wrapperDir}${path.delimiter}${env.PATH || ''}`;
        if (useFakeSessions) env.PATH = `${sessionDir}${path.delimiter}${env.PATH || ''}`;
        const worker = spawn(program, args, { ...options, env });
        workers.push({ worker, closed: new Promise(resolve => worker.once('close', resolve)) });
        return worker;
      }
    });
    const actionOptions = { lockKey: status.common_dir, validateTarget: () => true };
    const runMutation = async (method, params) => {
      try { return await mutations.execute(method, params, actionOptions); }
      finally { if (workers.length) await workers.at(-1).closed; }
    };
    const action = async (operation, target) => {
      const params = { repo: repoPath, path: target, operation };
      if (operation === 'discard' || operation === 'delete_untracked') {
        params.expected_token = (await client.fileActionPreview(repoPath, target, operation)).token;
      }
      return runMutation('file_action', params);
    };
    const literal = 'file[1].txt';
    const neighbor = 'file1.txt';
    write(literal, 'literal file\n');
    write(neighbor, 'neighbor\n');
    await action('stage', literal);
    assert.equal(git(repo, 'show', `:${literal}`), 'literal file');
    assert.equal(git(repo, 'status', '--porcelain', '--', neighbor).slice(0, 2), '??');
    await action('unstage', literal);
    assert.equal(git(repo, '--literal-pathspecs', 'status', '--porcelain', '--', literal).slice(0, 2), '??');
    assert.equal(git(repo, 'status', '--porcelain', '--', neighbor).slice(0, 2), '??');
    const stale = await client.fileActionPreview(repoPath, literal, 'delete_untracked');
    write(literal, 'new content after confirmation\n');
    await assert.rejects(runMutation('file_action', {
      repo: repoPath, path: literal, operation: 'delete_untracked', expected_token: stale.token
    }), error => error.code === 'file_changed');
    assert.equal(fs.readFileSync(path.join(repo, literal), 'utf8'), 'new content after confirmation\n');
    await action('delete_untracked', literal);
    assert.equal(fs.existsSync(path.join(repo, literal)), false);
    assert.equal(fs.readFileSync(path.join(repo, neighbor), 'utf8'), 'neighbor\n');
    assert.equal(git(repo, 'status', '--porcelain', '--', neighbor).slice(0, 2), '??');
    await action('discard', 'tracked.txt');
    assert.equal(fs.readFileSync(path.join(repo, 'tracked.txt'), 'utf8'), 'initial\n');
    assert.equal(git(repo, 'status', '--porcelain', '--', 'tracked.txt'), '');
    console.log('PASS real stage, unstage, stale confirmation rejection, untracked deletion and discard');

    const worktree = plannedWorktree.path;
    await runMutation('worktree_create', { repo: repoPath, path: worktree, branch: 'feature/verify' });
    assert.equal(git(worktree, 'branch', '--show-current'), 'feature/verify');
    if (process.platform !== 'win32') {
      const staleRemoval = await client.previewWorktreeRemoval(repoPath, worktree);
      const gitDir = git(worktree, 'rev-parse', '--absolute-git-dir');
      const savedGitDir = `${gitDir}-saved`;
      fs.renameSync(gitDir, savedGitDir);
      fs.cpSync(savedGitDir, gitDir, { recursive: true });
      try {
        await assert.rejects(runMutation('worktree_remove', { repo: repoPath, path: worktree,
          expected_repo_identity: staleRemoval.repo_identity, expected_worktree_identity: staleRemoval.worktree_identity }),
        error => error.code === 'stale_target');
        assert.equal(fs.existsSync(worktree), true);
      } finally {
        fs.rmSync(gitDir, { recursive: true, force: true });
        fs.renameSync(savedGitDir, gitDir);
      }
    }
    const removal = await client.previewWorktreeRemoval(repoPath, worktree);
    await runMutation('worktree_remove', { repo: repoPath, path: worktree,
      expected_repo_identity: removal.repo_identity, expected_worktree_identity: removal.worktree_identity });
    assert.equal(fs.existsSync(worktree), false);
    await assert.rejects(runMutation('worktree_create', {
      repo: repoPath, path: path.join(temporary, 'external-worktree'), branch: 'external-branch'
    }), error => error.code === 'outside_workspace');
    console.log('PASS configured worktree planning, workspace scope, creation and removal');

    if (process.platform !== 'win32') {
      fs.mkdirSync(sessionDir);
      const paneFile = path.join(temporary, 'session-panes.json');
      const focusFile = path.join(temporary, 'session-focused');
      const safeSession = 'tab $(not-a-command)';
      fs.writeFileSync(paneFile, JSON.stringify({ result: { panes: [
        { tab_id: safeSession, cwd: repoPath }, { tab_id: 'outside', cwd: outsidePath }
      ] } }));
      fs.writeFileSync(path.join(sessionDir, 'herdr'), '#!/bin/sh\nif [ "$1" = pane ] && [ "$2" = list ]; then /bin/cat "$GITPANE_TEST_PANES"; exit; fi\nif [ "$1" = tab ] && [ "$2" = focus ]; then printf %s "$3" > "$GITPANE_TEST_FOCUS"; exit; fi\nexit 1\n', { mode: 0o755 });
      gitEnv.GITPANE_TEST_PANES = paneFile;
      gitEnv.GITPANE_TEST_FOCUS = focusFile;
      const sessionEnv = { ...gitEnv, PATH: `${sessionDir}${path.delimiter}${gitEnv.PATH || ''}` };
      sessionEnv.HERDR_ENV = '1';
      delete sessionEnv.TMUX;
      delete sessionEnv.TMUX_PANE;
      sessionClient = new BridgeClient(executable, { spawn: (program, args, options) => {
        sessionChild = spawn(program, args, { ...options, env: sessionEnv });
        sessionClosed = new Promise(resolve => sessionChild.once('close', resolve));
        return sessionChild;
      } });
      await sessionClient.initialize([root]);
      assert.deepEqual(await sessionClient.sessionLiveness([repoPath]), {
        enabled: true, reason: null, repos: [{ repo: repoPath, live: true }]
      });
      await assert.rejects(sessionClient.sessionLiveness([outsidePath]), error => error.code === 'outside_workspace');
      assert.deepEqual(await sessionClient.call('sessions_list', { repo: repoPath, backend: 'herdr' }), { sessions: [safeSession] });
      await assert.rejects(sessionClient.call('sessions_list', { repo: outsidePath, backend: 'herdr' }), error => error.code === 'outside_workspace');
      useFakeSessions = true;
      await assert.rejects(runMutation('session_goto', { repo: repoPath, backend: 'herdr', session: 'outside' }), error => error.code === 'stale_target');
      assert.equal(fs.existsSync(focusFile), false);
      await runMutation('session_goto', { repo: repoPath, backend: 'herdr', session: safeSession });
      assert.equal(fs.readFileSync(focusFile, 'utf8'), safeSession);
      useFakeSessions = false;
      console.log('PASS scoped session discovery and literal one-shot navigation with fake host');

      const liveStarted = path.join(temporary, 'live-started');
      const liveRelease = path.join(temporary, 'live-release');
      const liveFinished = path.join(temporary, 'live-finished');
      const livePids = path.join(temporary, 'live-pids');
      fs.writeFileSync(path.join(sessionDir, 'herdr'), '#!/bin/sh\nprintf "%s\\n" "$$" >> "$GITPANE_LIVE_PIDS"\n: > "$GITPANE_LIVE_STARTED"\nwhile [ ! -e "$GITPANE_LIVE_RELEASE" ]; do /bin/sleep 0.02; done\n/bin/cat "$GITPANE_TEST_PANES"\n: > "$GITPANE_LIVE_FINISHED"\n', { mode: 0o755 });
      const liveEnv = { ...sessionEnv, GITPANE_LIVE_STARTED: liveStarted, GITPANE_LIVE_RELEASE: liveRelease,
        GITPANE_LIVE_FINISHED: liveFinished, GITPANE_LIVE_PIDS: livePids };
      class EventEmitter {
        constructor() { this.handlers = new Set(); this.event = handler => { this.handlers.add(handler); return { dispose: () => this.handlers.delete(handler) }; }; }
        fire(value) { for (const handler of this.handlers) handler(value); }
        dispose() { this.handlers.clear(); }
      }
      const event = () => ({ dispose() {} });
      const liveWorkers = [];
      const liveManager = new BackendManager({ extensionPath: temporary }, {
        vscode: { EventEmitter, workspace: { workspaceFolders: [{ uri: { scheme: 'file', fsPath: root } }],
          onDidChangeWorkspaceFolders: event, onDidChangeConfiguration: event,
          getConfiguration: () => ({ get: name => name === 'backendPath' ? executable : 'herdr' }) } },
        createClient: (program, options) => new BridgeClient(program, { ...options, spawn: (file, args, spawnOptions) => {
          const worker = spawn(file, args, { ...spawnOptions, env: liveEnv });
          liveWorkers.push(new Promise(resolve => worker.once('close', resolve)));
          return worker;
        } })
      });
      let published = 0;
      const liveTree = { sessionTargets: () => [{ path: repoPath, references: ['repo'] }], changed: () => { published++; } };
      const liveController = new SessionLiveness(liveTree, liveManager);
      try {
        await liveManager.ready();
        liveController.setVisible(true);
        await waitForFile(liveStarted);
        for (let index = 0; index < 4; index++) {
          liveController.setVisible(false);
          liveController.setVisible(true);
        }
        liveController.setVisible(false);
        assert.equal(fs.readFileSync(livePids, 'utf8').trim().split('\n').length, 1, 'one held host probe across visibility changes');
        fs.writeFileSync(liveRelease, '');
        await waitForFile(liveFinished);
        const deadline = Date.now() + 5000;
        while (liveController.pending) {
          if (Date.now() > deadline) throw new Error('Liveness probe did not drain');
          await new Promise(resolve => setTimeout(resolve, 10));
        }
        assert.equal(published, 0, 'a hidden stale probe must not publish');
        const pid = Number(fs.readFileSync(livePids, 'utf8').trim());
        assert.throws(() => process.kill(pid, 0), error => error.code === 'ESRCH', 'held host child must be reaped');
        liveController.setVisible(true);
        while (!liveController.live.get(repoPath)) {
          if (Date.now() > deadline) throw new Error('Liveness did not recover after hiding');
          await new Promise(resolve => setTimeout(resolve, 10));
        }
        assert.equal(fs.readFileSync(livePids, 'utf8').trim().split('\n').length, 2);
        while (liveController.pending) await new Promise(resolve => setTimeout(resolve, 10));
        liveController.setVisible(false);
        fs.unlinkSync(liveRelease);
        fs.unlinkSync(liveStarted);
        fs.unlinkSync(liveFinished);
        liveManager.livenessRequestTimeoutMs = 200;
        liveController.setVisible(true);
        await waitForFile(liveStarted);
        const timeoutDeadline = Date.now() + 5000;
        while (!liveController.error) {
          if (Date.now() > timeoutDeadline) throw new Error('Liveness timeout was not reported');
          await new Promise(resolve => setTimeout(resolve, 10));
        }
        assert.match(liveController.error, /timed out/i);
        const probePids = fs.readFileSync(livePids, 'utf8').trim().split('\n').map(Number);
        assert.equal(probePids.length, 3);
        assert.throws(() => process.kill(probePids[2], 0), error => error.code === 'ESRCH', 'timed-out host child must be reaped');
        assert.equal(liveManager.livenessClient, undefined, 'the worker closes after cleanup');
        assert.equal(fs.existsSync(liveFinished), false, 'the held host did not complete on its own');
      } finally {
        fs.writeFileSync(liveRelease, '');
        liveController.dispose();
        liveManager.dispose();
        await Promise.all(liveWorkers);
      }
      console.log('PASS held live-session probe drains after hiding without worker accumulation');

      write('in-flight.txt', 'pending\n');
      holdGit = true;
      const pending = action('stage', 'in-flight.txt');
      await waitForFile(gitStarted);
      generation++;
      mutations.dispose();
      fs.writeFileSync(release, '');
      await pending;
      assert.equal(git(repo, 'show', ':in-flight.txt'), 'pending');
      console.log('PASS dispatched mutation completes after workspace changes and disposal');
    } else console.log('SKIP delayed mutation on Windows: Unix shell wrapper required');
    if (stderr.length) process.stderr.write(stderr.join(''));
  } catch (error) {
    console.error(error.stack || String(error));
    process.exitCode = 1;
  } finally {
    if (configuredClient) configuredClient.dispose();
    if (configuredClosed) {
      let timer;
      try {
        const termination = await Promise.race([
          configuredClosed,
          new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Configured Gitpane service did not exit.')), 5000); })
        ]);
        if (termination.code !== 0 || termination.signal) {
          console.error(`Configured Gitpane service exited with code ${termination.code} and signal ${termination.signal}.`);
          process.exitCode = 1;
        }
      } catch (error) {
        configuredChild.kill('SIGKILL');
        console.error(error.message);
        process.exitCode = 1;
      } finally { clearTimeout(timer); }
    }
    if (sessionClient) sessionClient.dispose();
    if (sessionClosed) {
      let timer;
      try {
        await Promise.race([sessionClosed, new Promise((_, reject) => {
          timer = setTimeout(() => reject(new Error('Session service did not exit.')), 5000);
        })]);
      } catch (error) {
        if (sessionChild) sessionChild.kill('SIGKILL');
        console.error(error.message);
        process.exitCode = 1;
      } finally { clearTimeout(timer); }
    }
    if (mutations) mutations.dispose();
    fs.writeFileSync(release, '');
    for (const entry of workers) {
      let timer;
      try {
        await Promise.race([entry.closed, new Promise((_, reject) => {
          timer = setTimeout(() => reject(new Error('Mutation worker did not exit.')), 10000);
        })]);
      } catch (error) {
        console.error(error.message);
        process.exitCode = 1;
      } finally { clearTimeout(timer); }
    }
    if (client) client.dispose();
    if (closed) {
      let timer;
      try {
        const termination = await Promise.race([
          closed,
          new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Gitpane service did not exit after disposal.')), 5000); })
        ]);
        if (termination.code !== 0 || termination.signal) {
          console.error(`Gitpane service exited with code ${termination.code} and signal ${termination.signal}.`);
          process.exitCode = 1;
        }
      } catch (error) {
        child.kill('SIGKILL');
        console.error(error.message);
        process.exitCode = 1;
      } finally { clearTimeout(timer); }
    }
    fs.rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
  }
})();
