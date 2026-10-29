'use strict';

const os = require('node:os');

function isolatedGitEnv(base = process.env) {
  const env = { ...base, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: os.devNull };
  for (const key of Object.keys(env)) {
    if ([
      'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_PREFIX', 'GIT_COMMON_DIR',
      'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_NAMESPACE',
      'GIT_CONFIG_COUNT', 'GIT_CONFIG_PARAMETERS', 'GIT_TEMPLATE_DIR'
    ].includes(key) || /^GIT_CONFIG_(KEY|VALUE)_\d+$/.test(key)) delete env[key];
  }
  return env;
}

module.exports = { isolatedGitEnv };
