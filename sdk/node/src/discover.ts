import { readdir } from "node:fs/promises";
import { basename, extname, relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";

import type { ApiActionDefinition, ScheduledActionDefinition } from "./api.js";

interface ActionManifest {
  actions: unknown[];
}

const projectRoot = requiredArgValue("--project-root");
const sourceRoot = resolve(projectRoot, argValue("--source-root") ?? "src");

process.env.RYVUS_DISCOVER = "1";

const manifest: ActionManifest = { actions: [] };

for (const file of await sourceFiles(sourceRoot)) {
  const module = await import(`${pathToFileURL(file).href}?t=${Date.now()}`);
  const action = module.default;

  if (!isRyvusAction(action)) {
    continue;
  }

  if (action.type === "api") {
    const name = action.name ?? actionNameFromFile(file);

    manifest.actions.push({
      runtime: "Node",
      kind: {
        Api: {
          method: action.method,
          path: action.path,
          query_params: Object.entries(action.query).map(([name, schema]) => ({
            name,
            required: schema.required,
            schema: schema.jsonSchema,
          })),
          ...(action.body ? { request_schema: action.body.jsonSchema } : {}),
          ...(action.response ? { response_schema: action.response.jsonSchema } : {}),
        },
      },
      source: relative(projectRoot, file),
      entrypoint: "default",
      name,
      ...(action.policy ? { policy: action.policy } : {}),
    });
  } else {
    const name = action.name ?? actionNameFromFile(file);

    manifest.actions.push({
      runtime: "Node",
      kind: {
        Schedule: {
          expression: action.expression,
        },
      },
      source: relative(projectRoot, file),
      entrypoint: "default",
      name,
      ...(action.policy ? { policy: action.policy } : {}),
    });
  }
}

process.stdout.write(`${JSON.stringify(manifest, null, 2)}\n`);

function argValue(name: string): string | undefined {
  const index = process.argv.indexOf(name);

  if (index === -1) {
    return undefined;
  }

  return process.argv[index + 1];
}

function requiredArgValue(name: string): string {
  const value = argValue(name);

  if (value === undefined) {
    throw new Error(`${name} is required`);
  }

  return value;
}

async function sourceFiles(root: string): Promise<string[]> {
  const entries = await readdir(root, { withFileTypes: true });
  const files: string[] = [];

  for (const entry of entries) {
    const path = resolve(root, entry.name);

    if (entry.isDirectory()) {
      files.push(...await sourceFiles(path));
      continue;
    }

    if (entry.isFile() && [".js", ".mjs"].some((suffix) => path.endsWith(suffix))) {
      files.push(path);
    }
  }

  return files;
}

function isRyvusAction(
  value: unknown,
): value is ApiActionDefinition | ScheduledActionDefinition {
  return (
    typeof value === "object" &&
    value !== null &&
    (value as ApiActionDefinition).__ryvusAction === true &&
    ["api", "schedule"].includes((value as ApiActionDefinition).type)
  );
}

function actionNameFromFile(file: string): string {
  return basename(file, extname(file));
}
