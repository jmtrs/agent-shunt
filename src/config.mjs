import { access, readFile } from 'node:fs/promises';
import { constants } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const DEFAULTS = Object.freeze({
  model: 'qwen/qwen3.7-flash',
  baseUrl: 'https://openrouter.ai/api/v1',
  timeoutMs: 60_000,
  maxOutputTokens: 2_000,
  maxResponseBytes: 1_000_000,
  maxQuestionBytes: 16_000,
  maxRequestBytes: 4_000_000,
  maxFiles: 30,
  maxFileBytes: 512_000,
  maxTotalBytes: 2_000_000
});

async function exists(file) {
  try {
    await access(file, constants.R_OK);
    return true;
  } catch {
    return false;
  }
}

function parseEnv(text) {
  const values = {};
  for (const rawLine of text.split(/\r?\n/u)) {
    const line = rawLine.trim();
    if (!line || line.startsWith('#')) continue;
    const normalized = line.startsWith('export ') ? line.slice(7).trim() : line;
    const separator = normalized.indexOf('=');
    if (separator < 1) continue;
    const key = normalized.slice(0, separator).trim();
    let value = normalized.slice(separator + 1).trim();
    if ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'"))) {
      value = value.slice(1, -1);
    }
    values[key] = value;
  }
  return values;
}

async function readJsonIfPresent(file) {
  if (!(await exists(file))) return {};
  const parsed = JSON.parse(await readFile(file, 'utf8'));
  if (!parsed || Array.isArray(parsed) || typeof parsed !== 'object') {
    throw new Error(`Invalid configuration object: ${file}`);
  }
  return parsed;
}

function positiveInteger(value, name, fallback, hardMax = Number.MAX_SAFE_INTEGER) {
  if (value === undefined) return fallback;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) throw new Error(`${name} must be a positive integer`);
  if (parsed > hardMax) throw new Error(`${name} exceeds hard maximum ${hardMax}`);
  return parsed;
}

export async function resolveApiKey(env = process.env) {
  if (env.OPENROUTER_API_KEY?.trim()) {
    return { apiKey: env.OPENROUTER_API_KEY.trim(), source: 'environment' };
  }

  const home = os.homedir();
  const candidates = [
    env.AGENT_SHUNT_ENV_FILE,
    path.join(home, '.config', 'agent-shunt', '.env'),
    path.join(home, '.config', 'claude-openrouter', '.env'),
    path.join(home, '.config', 'or-info', '.env')
  ].filter(Boolean);

  for (const file of candidates) {
    if (!(await exists(file))) continue;
    const apiKey = parseEnv(await readFile(file, 'utf8')).OPENROUTER_API_KEY?.trim();
    if (apiKey) return { apiKey, source: file };
  }
  throw new Error('OPENROUTER_API_KEY not found in the environment or supported user config files');
}

export async function loadConfig(overrides = {}, env = process.env) {
  const configFile = env.AGENT_SHUNT_CONFIG || path.join(os.homedir(), '.config', 'agent-shunt', 'config.json');
  const stored = await readJsonIfPresent(configFile);
  const combined = { ...DEFAULTS, ...stored, ...overrides };

  const model = String(combined.model || '').trim();
  if (!model) throw new Error('model must not be empty');
  const parsedBaseUrl = new URL(String(combined.baseUrl || ''));
  if (parsedBaseUrl.origin !== 'https://openrouter.ai' || parsedBaseUrl.pathname.replace(/\/+$/u, '') !== '/api/v1' || parsedBaseUrl.username || parsedBaseUrl.password || parsedBaseUrl.search || parsedBaseUrl.hash) {
    throw new Error('baseUrl must be exactly https://openrouter.ai/api/v1');
  }
  const baseUrl = 'https://openrouter.ai/api/v1';

  return {
    model,
    baseUrl,
    timeoutMs: positiveInteger(combined.timeoutMs, 'timeoutMs', DEFAULTS.timeoutMs, 300_000),
    maxOutputTokens: positiveInteger(combined.maxOutputTokens, 'maxOutputTokens', DEFAULTS.maxOutputTokens, 8_192),
    maxResponseBytes: positiveInteger(combined.maxResponseBytes, 'maxResponseBytes', DEFAULTS.maxResponseBytes, 4_000_000),
    maxQuestionBytes: positiveInteger(combined.maxQuestionBytes, 'maxQuestionBytes', DEFAULTS.maxQuestionBytes, 64_000),
    maxRequestBytes: positiveInteger(combined.maxRequestBytes, 'maxRequestBytes', DEFAULTS.maxRequestBytes, 8_000_000),
    maxFiles: positiveInteger(combined.maxFiles, 'maxFiles', DEFAULTS.maxFiles, 200),
    maxFileBytes: positiveInteger(combined.maxFileBytes, 'maxFileBytes', DEFAULTS.maxFileBytes, 5_000_000),
    maxTotalBytes: positiveInteger(combined.maxTotalBytes, 'maxTotalBytes', DEFAULTS.maxTotalBytes, 8_000_000),
    configFile
  };
}

export { DEFAULTS };
