import { readFile } from "node:fs/promises";
import process from "node:process";

import { NreseClient } from "../lib/client";
import { cliHelpText, parseCliArgs } from "./args";

async function main(): Promise<void> {
  try {
    const rawArgs = process.argv.slice(2);
    if (
      rawArgs.length === 0 ||
      rawArgs[0] === "help" ||
      rawArgs[0] === "--help"
    ) {
      console.log(cliHelpText());
      return;
    }

    const options = parseCliArgs(rawArgs);
    const defaultHeaders = {
      ...(options.token ? { Authorization: `Bearer ${options.token}` } : {}),
      ...options.headers,
    };
    const client = new NreseClient({
      baseUrl: options.baseUrl,
      defaultHeaders,
    });

    switch (options.command.kind) {
      case "runtime":
        printJson(await client.getRuntimeSnapshot());
        break;
      case "capabilities":
        printJson(await client.getCapabilities());
        break;
      case "reasoning":
        printJson(await client.getReasoningDiagnostics());
        break;
      case "query": {
        const response = await client.runQuery(
          await resolveBody(options.command.text),
          options.command.accept,
        );
        printTextResponse(response.status, response.body);
        break;
      }
      case "update": {
        const response = await client.runUpdate(await resolveBody(options.command.text));
        printTextResponse(response.status, response.body);
        break;
      }
      case "tell": {
        const response = await client.runTell(
          await resolveBody(options.command.text),
          options.command.graphMode,
          options.command.graphIri,
        );
        printTextResponse(response.status, response.body);
        break;
      }
      case "graph-read": {
        const response = await client.readGraph(
          options.command.graphMode,
          options.command.graphIri,
        );
        printTextResponse(response.status, response.body);
        break;
      }
      case "graph-write": {
        const response = await client.writeGraph(
          options.command.method,
          await resolveBody(options.command.text),
          options.command.graphMode,
          options.command.graphIri,
        );
        printTextResponse(response.status, response.body);
        break;
      }
      case "graph-delete": {
        const response = await client.deleteGraph(
          options.command.graphMode,
          options.command.graphIri,
        );
        printTextResponse(response.status, response.body);
        break;
      }
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(message);
    process.exitCode = 1;
  }
}

async function resolveBody(input: string): Promise<string> {
  if (!input.startsWith("@file:")) {
    return input;
  }
  const path = input.slice("@file:".length);
  return readFile(path, "utf8");
}

function printJson(value: unknown): void {
  console.log(JSON.stringify(value, null, 2));
}

function printTextResponse(status: number, body: string): void {
  if (!body) {
    console.log(`status=${status}`);
    return;
  }
  console.log(`status=${status}`);
  console.log(body);
}

void main();
