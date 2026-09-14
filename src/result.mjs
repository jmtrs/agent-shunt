const MAX_ANSWER_LENGTH = 16_000;
const MAX_SUMMARY_LENGTH = 2_000;
const MAX_UNCERTAINTY_LENGTH = 2_000;
const MAX_FINDINGS = 100;
const MAX_UNCERTAINTIES = 50;

function requireString(value, field, maxLength) {
  if (typeof value !== 'string') throw new Error(`model response field ${field} must be a string`);
  if (value.length > maxLength) throw new Error(`model response field ${field} exceeds ${maxLength} characters`);
  return value;
}

function requireInteger(value, field) {
  if (!Number.isSafeInteger(value)) throw new Error(`model response field ${field} must be an integer`);
  return value;
}

export function parseAndValidateResult(raw, documents) {
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new Error('model returned invalid JSON');
  }
  if (!parsed || Array.isArray(parsed) || typeof parsed !== 'object') throw new Error('model response must be an object');
  if (!Array.isArray(parsed.findings) || !Array.isArray(parsed.uncertainties)) {
    throw new Error('model response must contain findings and uncertainties arrays');
  }
  if (parsed.findings.length > MAX_FINDINGS || parsed.uncertainties.length > MAX_UNCERTAINTIES) throw new Error('model response contains too many items');

  const byPath = new Map(documents.map((document) => [document.path, document]));
  const findings = parsed.findings.map((finding, index) => {
    if (!finding || Array.isArray(finding) || typeof finding !== 'object') throw new Error(`finding ${index} must be an object`);
    const filePath = requireString(finding.path, `findings[${index}].path`, 1_024);
    const document = byPath.get(filePath);
    if (!document) throw new Error(`finding ${index} references an unknown path: ${filePath}`);
    const startLine = requireInteger(finding.startLine, `findings[${index}].startLine`);
    const endLine = requireInteger(finding.endLine, `findings[${index}].endLine`);
    if (startLine < 1 || endLine < startLine || endLine > document.lineCount) {
      throw new Error(`finding ${index} has an invalid line range for ${filePath}`);
    }
    return {
      path: filePath,
      startLine,
      endLine,
      summary: requireString(finding.summary, `findings[${index}].summary`, MAX_SUMMARY_LENGTH)
    };
  });

  return {
    version: 1,
    answer: requireString(parsed.answer, 'answer', MAX_ANSWER_LENGTH),
    findings,
    uncertainties: parsed.uncertainties.map((item, index) => requireString(item, `uncertainties[${index}]`, MAX_UNCERTAINTY_LENGTH)),
    filesRead: documents.map((document) => document.path),
    trust: 'untrusted-model-output-with-validated-source-references'
  };
}

export const RESULT_SCHEMA = {
  name: 'agent_shunt_scan_result',
  strict: true,
  schema: {
    type: 'object',
    additionalProperties: false,
    required: ['answer', 'findings', 'uncertainties'],
    properties: {
      answer: { type: 'string', maxLength: MAX_ANSWER_LENGTH },
      findings: {
        type: 'array',
        maxItems: MAX_FINDINGS,
        items: {
          type: 'object',
          additionalProperties: false,
          required: ['path', 'startLine', 'endLine', 'summary'],
          properties: {
            path: { type: 'string', maxLength: 1024 },
            startLine: { type: 'integer', minimum: 1 },
            endLine: { type: 'integer', minimum: 1 },
            summary: { type: 'string', maxLength: MAX_SUMMARY_LENGTH }
          }
        }
      },
      uncertainties: { type: 'array', maxItems: MAX_UNCERTAINTIES, items: { type: 'string', maxLength: MAX_UNCERTAINTY_LENGTH } }
    }
  }
};
