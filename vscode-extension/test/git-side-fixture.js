'use strict';

function installGitSideFixture(backend, findRepo) {
  backend.gitSideSession = async repo => ({ repo, git: { key: 'git', executable: '/usr/bin/git' },
    snapshot: { roots: [repo], rootSources: [repo], rootIdentities: ['f'.repeat(64)] } });
  backend.gitSideCurrent = () => true;
  backend.gitSideOperation = async (session, identity, operation) => {
    if (operation.kind === 'verified_content') {
      const metadata = await backend.repoBlobMetadata(session.repo, identity,
        operation.ref === ':0' ? '' : operation.ref, operation.path);
      return findRepo(session.repo).buffer(operation.ref === 'HEAD' ? metadata.head_oid : operation.ref, operation.path);
    }
    const results = [];
    for (const item of operation.items) {
      const ref = item.ref === ':0' ? '' : item.ref;
      try { results.push({ metadata: await backend.repoBlobMetadata(session.repo, identity, ref, item.path) }); }
      catch (error) {
        if (!error?.code) throw error;
        results.push({ error: { code: error.code, message: error.message } });
      }
    }
    return { version: 1, volatile: false, replacements: 'a'.repeat(64), config: 'b'.repeat(64), results };
  };
}

module.exports = { installGitSideFixture };
