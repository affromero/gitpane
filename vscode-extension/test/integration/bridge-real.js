'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');
const { BridgeClient } = require('../../src/bridge-client');
const { MutationManager } = require('../../src/mutation-manager');
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
  let sessionClient, sessionChild, sessionClosed;
  let mutations;
  const workers = [];
  const release = path.join(temporary, 'release-git');
  try {
    if (!executable || !path.isAbsolute(executable) || !fs.existsSync(executable)) {
      throw new Error('Set GITPANE_TEST_BINARY to the absolute path of the built Gitpane service.');
    }
    for (const directory of [repo, outside, hooks]) fs.mkdirSync(directory, { recursive: true });
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
    const root = fs.realpathSync(workspace);
    const repoPath = fs.realpathSync(repo);
    const outsidePath = fs.realpathSync(outside);
    assert.deepEqual(await client.initialize([root]), { version: 1 });

    const status = await client.status(repoPath);
    assert.equal(status.is_dirty, true);
    const tracked = status.files.find(file => file.path === 'tracked.txt');
    assert.ok(tracked && tracked.staged && tracked.unstaged, 'staged and unstaged target must be retained');
    assert.ok(status.files.some(file => file.path === 'new file.txt' && file.status === 'untracked'));
    console.log('PASS handshake and real repository status');

    const graph = await client.graph(repoPath, false);
    const merge = graph.rows.find(row => row.oid === mergeOid);
    assert.ok(merge && merge.is_merge && merge.parent_oids.length === 2, 'merge commit and parents must appear');
    assert.ok(graph.rows.some(row => row.oid === rootOid), 'root commit must appear');
    assert.ok(merge.lanes.length > 0, 'graph lanes must be provided');
    const firstParent = await client.graph(repoPath, true);
    assert.ok(firstParent.rows.some(row => row.oid === mergeOid), 'first-parent graph must include the merge');
    console.log('PASS graph and first-parent request');

    const rootFiles = await client.commitFiles(repoPath, rootOid);
    assert.equal(rootFiles.message, 'root commit');
    assert.ok(rootFiles.files.some(file => file.path === 'tracked.txt' && file.status === 'A' && !file.is_submodule));
    const mergeFiles = await client.commitFiles(repoPath, mergeOid);
    assert.equal(mergeFiles.message, 'merge topic');
    assert.ok(mergeFiles.files.some(file => file.path === 'topic.txt' && file.status === 'A' && !file.is_submodule));
    console.log('PASS root and merge commit files');

    for (const request of [() => client.status(outsidePath), () => client.graph(outsidePath),
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
    mutations = new MutationManager({
      getSnapshot: () => ({ executable, roots: [root], generation }),
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
    const literal = ':(glob)*.txt';
    write(literal, 'literal file\n');
    await action('stage', literal);
    assert.equal(git(repo, 'show', `:${literal}`), 'literal file');
    await action('unstage', literal);
    assert.equal(git(repo, '--literal-pathspecs', 'status', '--porcelain', '--', literal).slice(0, 2), '??');
    const stale = await client.fileActionPreview(repoPath, literal, 'delete_untracked');
    write(literal, 'new content after confirmation\n');
    await assert.rejects(runMutation('file_action', {
      repo: repoPath, path: literal, operation: 'delete_untracked', expected_token: stale.token
    }), error => error.code === 'file_changed');
    assert.equal(fs.readFileSync(path.join(repo, literal), 'utf8'), 'new content after confirmation\n');
    await action('delete_untracked', literal);
    assert.equal(fs.existsSync(path.join(repo, literal)), false);
    await action('discard', 'tracked.txt');
    assert.equal(fs.readFileSync(path.join(repo, 'tracked.txt'), 'utf8'), 'initial\n');
    assert.equal(git(repo, 'status', '--porcelain', '--', 'tracked.txt'), '');
    console.log('PASS real stage, unstage, stale confirmation rejection, untracked deletion and discard');

    const worktree = path.join(root, 'new-worktree');
    await runMutation('worktree_create', { repo: repoPath, path: worktree, branch: 'bridge-worktree' });
    assert.equal(git(worktree, 'branch', '--show-current'), 'bridge-worktree');
    await runMutation('worktree_remove', { repo: repoPath, path: worktree });
    assert.equal(fs.existsSync(worktree), false);
    await assert.rejects(runMutation('worktree_create', {
      repo: repoPath, path: path.join(temporary, 'external-worktree'), branch: 'external-branch'
    }), error => error.code === 'outside_workspace');
    console.log('PASS workspace worktree creation, removal and outside rejection');

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
      sessionClient = new BridgeClient(executable, { spawn: (program, args, options) => {
        sessionChild = spawn(program, args, { ...options, env: sessionEnv });
        sessionClosed = new Promise(resolve => sessionChild.once('close', resolve));
        return sessionChild;
      } });
      await sessionClient.initialize([root]);
      assert.deepEqual(await sessionClient.call('sessions_list', { repo: repoPath, backend: 'herdr' }), { sessions: [safeSession] });
      await assert.rejects(sessionClient.call('sessions_list', { repo: outsidePath, backend: 'herdr' }), error => error.code === 'outside_workspace');
      useFakeSessions = true;
      await assert.rejects(runMutation('session_goto', { repo: repoPath, backend: 'herdr', session: 'outside' }), error => error.code === 'stale_target');
      assert.equal(fs.existsSync(focusFile), false);
      await runMutation('session_goto', { repo: repoPath, backend: 'herdr', session: safeSession });
      assert.equal(fs.readFileSync(focusFile, 'utf8'), safeSession);
      useFakeSessions = false;
      console.log('PASS scoped session discovery and literal one-shot navigation with fake host');

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
