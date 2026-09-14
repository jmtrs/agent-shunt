import assert from 'node:assert/strict';
import { mkdtemp, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { loadConfig, resolveApiKey } from '../src/config.mjs';

test('environment API key wins and is not transformed', async () => {
  const found = await resolveApiKey({ OPENROUTER_API_KEY: 'secret-value' });
  assert.deepEqual(found, { apiKey: 'secret-value', source: 'environment' });
});

test('explicit env file is supported', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'agent-shunt-'));
  const envFile = path.join(directory, '.env');
  await writeFile(envFile, 'OPENROUTER_API_KEY="from-file"\n');
  const found = await resolveApiKey({ AGENT_SHUNT_ENV_FILE: envFile });
  assert.deepEqual(found, { apiKey: 'from-file', source: envFile });
});

test('invalid numeric config fails closed', async () => {
  await assert.rejects(loadConfig({ maxFiles: 0 }, { AGENT_SHUNT_CONFIG: '/file/does/not/exist' }), /positive integer/u);
  await assert.rejects(loadConfig({ maxFiles: 201 }, { AGENT_SHUNT_CONFIG: '/file/does/not/exist' }), /hard maximum/u);
});

test('rejects a custom API origin to protect the Bearer token', async () => {
  await assert.rejects(loadConfig({ baseUrl: 'https://attacker.example/api/v1' }, { AGENT_SHUNT_CONFIG: '/file/does/not/exist' }), /exactly https:\/\/openrouter\.ai/u);
});
