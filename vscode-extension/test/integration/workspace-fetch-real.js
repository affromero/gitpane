'use strict';

const assert = require('node:assert/strict');
const { setTimeout: delay } = require('node:timers/promises');

module.exports = async function verifyAutomaticFetch({ vscode, git, gitAt, mainWriter, snapshots }) {
  const settings = vscode.workspace.getConfiguration('gitpane');
  const localHead = git('rev-parse', 'HEAD');
  const oldRemote = git('rev-parse', 'origin/main');
  git('branch', '--set-upstream-to=origin/main');
  gitAt(mainWriter, 'commit', '--allow-empty', '-qm', 'advance origin from another clone');
  gitAt(mainWriter, 'push', '-q');
  assert.equal(git('rev-parse', 'origin/main'), oldRemote);
  const expected = Number(gitAt(mainWriter, 'rev-list', '--count', `${localHead}..HEAD`));

  await settings.update('fetchInterval', 1);
  try {
    const deadline = Date.now() + 15000;
    const behind = () => snapshots.at(-1)?.repositories.find(item => item.name === 'repo')?.behind;
    while (behind() !== expected && Date.now() < deadline) await delay(100);
    assert.equal(behind(), expected, 'an automatic fetch updates the visible upstream count');
    assert.equal(git('rev-parse', 'origin/main'), gitAt(mainWriter, 'rev-parse', 'HEAD'));
    assert.equal(git('rev-parse', 'HEAD'), localHead, 'fetch leaves the current branch at the same commit');
  } finally {
    await settings.update('fetchInterval', 0);
  }
  console.log('PASS automatic fetch updates the pull count after a push from another clone');
};
