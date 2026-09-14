import { scan } from './scan.mjs';

const HELP = `agent-shunt — read-only bulk context worker

Usage:
  agent-shunt scan --question <text> --path <file> [--path <file> ...]
  agent-shunt check

Options:
  -q, --question <text>    Analysis question
  -p, --path <file>        Source file; repeat for multiple files
  -m, --model <id>         Override configured OpenRouter model
      --dry-run            Validate and size inputs without an API request
      --cwd <directory>    Root for paths (defaults to current directory)
  -h, --help               Show this help
`;

function parseArgs(argv) {
  const command = argv[0] && !argv[0].startsWith('-') ? argv[0] : 'help';
  const options = { paths: [], cwd: process.cwd(), configOverrides: {} };
  for (let index = 1; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--dry-run') options.dryRun = true;
    else if (arg === '--help' || arg === '-h') options.help = true;
    else if (arg === '--question' || arg === '-q') options.question = argv[++index];
    else if (arg === '--path' || arg === '-p') options.paths.push(argv[++index]);
    else if (arg === '--model' || arg === '-m') options.configOverrides.model = argv[++index];
    else if (arg === '--cwd') options.cwd = argv[++index];
    else throw new Error(`unknown argument: ${arg}`);
  }
  return { command, options };
}

export async function runCli(argv) {
  const { command, options } = parseArgs(argv);
  if (command === 'help' || options.help) {
    process.stdout.write(HELP);
    return;
  }
  if (command === 'check') {
    const { loadConfig, resolveApiKey } = await import('./config.mjs');
    const config = await loadConfig(options.configOverrides);
    const credential = await resolveApiKey();
    process.stdout.write(`${JSON.stringify({ ok: true, model: config.model, credentialSource: credential.source }, null, 2)}\n`);
    return;
  }
  if (command !== 'scan') throw new Error(`unknown command: ${command}`);
  const result = await scan(options);
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

export { parseArgs };
