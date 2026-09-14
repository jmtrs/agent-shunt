import assert from 'node:assert/strict';
import { mkdtemp, mkdir, symlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { loadDocuments } from '../src/files.mjs';

const limits = { maxFiles: 3, maxFileBytes: 1000, maxTotalBytes: 2000 };

test('loads text with stable relative paths and line numbers', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'agent-shunt-'));
  await mkdir(path.join(root, 'src'));
  await writeFile(path.join(root, 'src', 'a.js'), 'one\ntwo');
  const loaded = await loadDocuments(['src/a.js'], { ...limits, cwd: root });
  assert.equal(loaded.documents[0].path, 'src/a.js');
  assert.equal(loaded.documents[0].numberedContent, '1: one\n2: two');
});

test('rejects symlinks escaping the working directory', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'agent-shunt-root-'));
  const outside = await mkdtemp(path.join(os.tmpdir(), 'agent-shunt-outside-'));
  await writeFile(path.join(outside, 'secret.txt'), 'secret');
  await symlink(path.join(outside, 'secret.txt'), path.join(root, 'escape.txt'));
  await assert.rejects(loadDocuments(['escape.txt'], { ...limits, cwd: root }), /escapes working directory/u);
});

test('rejects binary files', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'agent-shunt-'));
  await writeFile(path.join(root, 'binary'), Buffer.from([1, 0, 2]));
  await assert.rejects(loadDocuments(['binary'], { ...limits, cwd: root }), /binary file rejected/u);
});
