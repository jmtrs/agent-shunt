import { open, realpath } from 'node:fs/promises';
import { constants } from 'node:fs';
import path from 'node:path';

function isInside(root, candidate) {
  const relative = path.relative(root, candidate);
  return relative === '' || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative));
}

function looksBinary(buffer) {
  const sampleLength = Math.min(buffer.length, 8_192);
  for (let index = 0; index < sampleLength; index += 1) {
    if (buffer[index] === 0) return true;
  }
  return false;
}

function numberLines(text) {
  return text.split(/\r?\n/u).map((line, index) => `${index + 1}: ${line}`).join('\n');
}

export async function loadDocuments(inputPaths, options) {
  if (!Array.isArray(inputPaths) || inputPaths.length === 0) throw new Error('at least one --path is required');
  if (inputPaths.length > options.maxFiles) throw new Error(`too many files: ${inputPaths.length} exceeds ${options.maxFiles}`);

  const root = await realpath(options.cwd);
  const seen = new Set();
  const documents = [];
  let totalBytes = 0;

  for (const inputPath of inputPaths) {
    const resolved = await realpath(path.resolve(root, inputPath));
    if (!isInside(root, resolved)) throw new Error(`path escapes working directory: ${inputPath}`);
    if (seen.has(resolved)) continue;
    seen.add(resolved);

    const handle = await open(resolved, constants.O_RDONLY | (constants.O_NOFOLLOW || 0));
    let metadata;
    let buffer;
    try {
      metadata = await handle.stat();
      if (!metadata.isFile()) throw new Error(`path is not a regular file: ${inputPath}`);
      if (metadata.size > options.maxFileBytes) throw new Error(`file exceeds ${options.maxFileBytes} bytes: ${inputPath}`);
      buffer = await handle.readFile();
      const afterRead = await handle.stat();
      if (metadata.dev !== afterRead.dev || metadata.ino !== afterRead.ino || metadata.size !== afterRead.size || buffer.byteLength !== afterRead.size) {
        throw new Error(`file changed while being read: ${inputPath}`);
      }
    } finally {
      await handle.close();
    }
    const canonicalAfterRead = await realpath(resolved);
    if (canonicalAfterRead !== resolved || !isInside(root, canonicalAfterRead)) throw new Error(`path changed while being read: ${inputPath}`);
    if (buffer.byteLength > options.maxFileBytes) throw new Error(`file exceeds ${options.maxFileBytes} bytes: ${inputPath}`);
    totalBytes += buffer.byteLength;
    if (totalBytes > options.maxTotalBytes) throw new Error(`input exceeds aggregate limit of ${options.maxTotalBytes} bytes`);
    if (looksBinary(buffer)) throw new Error(`binary file rejected: ${inputPath}`);
    const text = buffer.toString('utf8');
    const relativePath = path.relative(root, resolved) || path.basename(resolved);
    documents.push({
      path: relativePath,
      absolutePath: resolved,
      bytes: buffer.byteLength,
      lineCount: text.split(/\r?\n/u).length,
      numberedContent: numberLines(text)
    });
  }

  return { root, documents, totalBytes };
}
