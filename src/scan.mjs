import { loadConfig, resolveApiKey } from './config.mjs';
import { loadDocuments } from './files.mjs';
import { callOpenRouter } from './openrouter.mjs';
import { parseAndValidateResult } from './result.mjs';

export async function scan(request, dependencies = {}) {
  const question = String(request.question || '').trim();
  if (!question) throw new Error('question must not be empty');
  const config = await (dependencies.loadConfig || loadConfig)(request.configOverrides);
  if (Buffer.byteLength(question) > config.maxQuestionBytes) throw new Error(`question exceeds ${config.maxQuestionBytes} bytes`);
  const loaded = await (dependencies.loadDocuments || loadDocuments)(request.paths, { ...config, cwd: request.cwd });

  if (request.dryRun) {
    return {
      version: 1,
      dryRun: true,
      model: config.model,
      filesRead: loaded.documents.map((document) => document.path),
      totalBytes: loaded.totalBytes
    };
  }

  const credential = await (dependencies.resolveApiKey || resolveApiKey)();
  const response = await (dependencies.callOpenRouter || callOpenRouter)({
    apiKey: credential.apiKey,
    baseUrl: config.baseUrl,
    model: config.model,
    question,
    documents: loaded.documents,
    timeoutMs: config.timeoutMs,
    maxOutputTokens: config.maxOutputTokens,
    maxResponseBytes: config.maxResponseBytes,
    maxRequestBytes: config.maxRequestBytes
  });
  const result = parseAndValidateResult(response.content, loaded.documents);
  return { ...result, model: response.responseModel, usage: response.usage };
}
