// Tauri beforeBuild/beforeDev hook. Bundle a native stdio executable, no JS runtime at use time.
import { spawnSync } from 'node:child_process'
import { copyFileSync, mkdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const appDir = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const manifest = resolve(appDir, '../mcp/Cargo.toml')
function run(command, args, capture = false) {
  const result = spawnSync(command, args, {
    cwd: appDir,
    encoding: 'utf8',
    stdio: capture ? ['ignore', 'pipe', 'inherit'] : 'inherit',
  })
  if (result.error) throw result.error
  if (result.status !== 0) throw new Error(`${command} exited with ${result.status}`)
  return result.stdout
}

const target = process.env.TAURI_ENV_TARGET_TRIPLE || run('rustc', ['--print', 'host-tuple'], true).trim()
const debug = process.argv.includes('--debug') || process.env.TAURI_ENV_DEBUG === 'true'
const profile = debug ? 'debug' : 'release'
const metadata = JSON.parse(run('cargo', ['metadata', '--no-deps', '--format-version', '1', '--manifest-path', manifest], true))
const binDir = join(appDir, 'binaries')
mkdirSync(binDir, { recursive: true })

function build(triple) {
  run('cargo', ['build', '--locked', '--manifest-path', manifest, '--bin', 'weimo-douban-mcp', '--target', triple, ...(debug ? [] : ['--release'])])
  return join(metadata.target_directory, triple, profile, `weimo-douban-mcp${triple.includes('windows') ? '.exe' : ''}`)
}

if (target === 'universal-apple-darwin') {
  const inputs = ['aarch64-apple-darwin', 'x86_64-apple-darwin'].map(build)
  run('lipo', ['-create', ...inputs, '-output', join(binDir, `weimo-douban-mcp-${target}`)])
} else {
  copyFileSync(build(target), join(binDir, `weimo-douban-mcp-${target}${target.includes('windows') ? '.exe' : ''}`))
}
