#!/usr/bin/env npx tsx
/**
 * Push game configs to IC canister using dfx CLI
 * 
 * Usage:
 *   pnpm push-config <version>       # Push specific version
 *   pnpm push-config --all           # Push all configs
 *   pnpm push-config --set <version> # Set current config version
 *   pnpm push-config --list          # List configs on IC
 * 
 * Uses current dfx identity and canister_ids.json
 */

import { readFileSync, readdirSync, existsSync } from 'fs'
import { join, dirname } from 'path'
import { fileURLToPath } from 'url'
import { execSync } from 'child_process'

const __dirname = dirname(fileURLToPath(import.meta.url))
const CONFIGS_DIR = join(__dirname, '..', 'configs')
const PROJECT_DIR = join(__dirname, '..')

let USE_PROD = false

function getCanisterId(): string {
  const idsPath = join(PROJECT_DIR, 'canister_ids.json')
  if (existsSync(idsPath)) {
    const ids = JSON.parse(readFileSync(idsPath, 'utf-8'))
    const key = USE_PROD ? 'orbs_backend_prod' : 'orbs_backend'
    const canister = ids[key]
    if (canister?.ic) return canister.ic
  }
  throw new Error(`Could not find canister ID for ${USE_PROD ? 'orbs_backend_prod' : 'orbs_backend'} in canister_ids.json`)
}

function dfx(method: string, args: string = ''): string {
  const canisterId = getCanisterId()
  const cmd = `dfx canister --network ic call ${canisterId} ${method} ${args}`
  console.log(`> ${cmd}`)
  return execSync(cmd, { encoding: 'utf-8', cwd: PROJECT_DIR }).trim()
}

function getConfigFiles(): { version: string; filePath: string }[] {
  return readdirSync(CONFIGS_DIR)
    .filter((f: string) => f.startsWith('gameConfig.v') && f.endsWith('.json'))
    .map((f: string) => {
      const match = f.match(/gameConfig\.v(.+)\.json/)
      return { version: match ? match[1] : '', filePath: join(CONFIGS_DIR, f) }
    })
    .filter((c: { version: string }) => c.version)
    .sort((a: { version: string }, b: { version: string }) => 
      a.version.localeCompare(b.version, undefined, { numeric: true }))
}

function pushConfig(version: string, forceUpdate: boolean = false): boolean {
  const configPath = join(CONFIGS_DIR, `gameConfig.v${version}.json`)

  if (!existsSync(configPath)) {
    console.error(`Config file not found: ${configPath}`)
    return false
  }

  const configJson = readFileSync(configPath, 'utf-8')
  JSON.parse(configJson) // Validate JSON

  // Escape the JSON for shell - use base64 to avoid escaping issues
  const escaped = configJson.replace(/\\/g, '\\\\').replace(/"/g, '\\"')
  
  const method = forceUpdate ? 'update_engine_config' : 'add_engine_config'
  console.log(`\n${forceUpdate ? 'Updating' : 'Pushing'} config v${version}...`)
  try {
    const result = dfx(method, `'("${version}", "${escaped}")'`)
    console.log(result)
    if (result.includes('Err')) {
      // If add failed because it exists and we're not forcing, try update
      if (!forceUpdate && result.includes('already exists')) {
        console.log('Config exists, trying update...')
        return pushConfig(version, true)
      }
      return false
    }
    console.log(`Done: v${version} ${forceUpdate ? 'updated' : 'pushed'}`)
    return true
  } catch (e: any) {
    console.error(`Failed: ${e.message}`)
    return false
  }
}

function listConfigs(): void {
  console.log('\nFetching configs from IC...')
  const result = dfx('list_engine_config_versions', "'()'")
  console.log(result)
  
  const currentResult = dfx('get_current_config_version', "'()'")
  console.log(`\nCurrent version: ${currentResult}`)
}

function setCurrentVersion(version: string): void {
  console.log(`\nSetting current version to v${version}...`)
  const result = dfx('set_current_config_version', `'("${version}")'`)
  console.log(result)
}

function main(): void {
  const args = process.argv.slice(2)

  if (args.length === 0) {
    console.log(`
Usage:
  pnpm push-config [--prod] <version>         # Push specific version (e.g., 1.2.2)
  pnpm push-config [--prod] --all             # Push all configs
  pnpm push-config [--prod] --update <version> # Force update existing config
  pnpm push-config [--prod] --set <version>   # Set current version
  pnpm push-config [--prod] --list            # List configs on IC

  --prod targets orbs_backend_prod canister

Local configs:`)
    for (const c of getConfigFiles()) console.log(`  v${c.version}`)
    return
  }

  if (args.includes('--prod')) {
    USE_PROD = true
    args.splice(args.indexOf('--prod'), 1)
    console.log('Targeting: orbs_backend_prod')
  } else {
    console.log('Targeting: orbs_backend (dev)')
  }

  if (args.length === 0) {
    console.log('No command specified after --prod. Use --all, --list, --set, or a version.')
    return
  }

  if (args[0] === '--list') {
    listConfigs()
  } else if (args[0] === '--all') {
    for (const c of getConfigFiles()) {
      pushConfig(c.version)
    }
  } else if (args[0] === '--update') {
    if (!args[1]) {
      console.error('Missing version argument')
      process.exit(1)
    }
    pushConfig(args[1], true)
  } else if (args[0] === '--set') {
    if (!args[1]) {
      console.error('Missing version argument')
      process.exit(1)
    }
    setCurrentVersion(args[1])
  } else {
    pushConfig(args[0])
  }
}

main()
