export type CliCommand =
  | { kind: "runtime" }
  | { kind: "capabilities" }
  | { kind: "reasoning" }
  | { kind: "query"; text: string; accept: string }
  | { kind: "update"; text: string }
  | { kind: "tell"; text: string; graphMode: "default" | "named"; graphIri: string }
  | { kind: "graph-read"; graphMode: "default" | "named"; graphIri: string }
  | {
      kind: "graph-write";
      method: "PUT" | "POST";
      text: string;
      graphMode: "default" | "named";
      graphIri: string;
    }
  | { kind: "graph-delete"; graphMode: "default" | "named"; graphIri: string };

export type CliOptions = {
  baseUrl: string;
  token?: string;
  headers: Record<string, string>;
  command: CliCommand;
};

type PendingOptions = {
  baseUrl: string;
  token?: string;
  headers: Record<string, string>;
  text?: string;
  file?: string;
  accept: string;
  graphMode: "default" | "named";
  graphIri: string;
};

const HELP_TEXT = `Usage:
  npm run cli -- runtime
  npm run cli -- capabilities
  npm run cli -- reasoning
  npm run cli -- query --text "SELECT * WHERE { ?s ?p ?o } LIMIT 5"
  npm run cli -- update --file .\\change.ru
  npm run cli -- tell --file .\\data.ttl --graph named --graph-iri https://example.org/g
  npm run cli -- graph-read --graph named --graph-iri https://example.org/g
  npm run cli -- graph-write --method PUT --file .\\data.ttl --graph named --graph-iri https://example.org/g
  npm run cli -- graph-delete --graph named --graph-iri https://example.org/g

Global options:
  --base-url <url>     Backend base URL. Defaults to NRESE_API_BASE_URL or http://127.0.0.1:8080
  --token <token>      Bearer token. Defaults to NRESE_API_TOKEN
  --header <k:v>       Additional header. May be repeated.

Content options:
  --text <text>        Inline SPARQL/RDF payload
  --file <path>        Read SPARQL/RDF payload from file
  --accept <type>      Query accept header. Default: application/sparql-results+json
  --graph <mode>       default | named
  --graph-iri <iri>    Named graph IRI when --graph named
  --method <verb>      PUT | POST for graph-write`;

function requireValue(args: string[], index: number, flag: string): string {
  const value = args[index + 1];
  if (!value) {
    throw new Error(`Missing value for ${flag}`);
  }
  return value;
}

function parseHeader(value: string): [string, string] {
  const separator = value.indexOf(":");
  if (separator <= 0) {
    throw new Error(`Invalid header value "${value}". Expected name:value.`);
  }
  return [value.slice(0, separator).trim(), value.slice(separator + 1).trim()];
}

export function cliHelpText(): string {
  return HELP_TEXT;
}

export function parseCliArgs(argv: string[]): CliOptions {
  const [commandName, ...rest] = argv;
  if (!commandName || commandName === "--help" || commandName === "help") {
    throw new Error(HELP_TEXT);
  }

  const pending: PendingOptions = {
    baseUrl: process.env.NRESE_API_BASE_URL ?? "http://127.0.0.1:8080",
    token: process.env.NRESE_API_TOKEN,
    headers: {},
    accept: "application/sparql-results+json",
    graphMode: "default",
    graphIri: "",
  };

  let method: "PUT" | "POST" = "PUT";

  for (let index = 0; index < rest.length; index += 1) {
    const arg = rest[index];
    switch (arg) {
      case "--base-url":
        pending.baseUrl = requireValue(rest, index, arg);
        index += 1;
        break;
      case "--token":
        pending.token = requireValue(rest, index, arg);
        index += 1;
        break;
      case "--header": {
        const [name, value] = parseHeader(requireValue(rest, index, arg));
        pending.headers[name] = value;
        index += 1;
        break;
      }
      case "--text":
        pending.text = requireValue(rest, index, arg);
        index += 1;
        break;
      case "--file":
        pending.file = requireValue(rest, index, arg);
        index += 1;
        break;
      case "--accept":
        pending.accept = requireValue(rest, index, arg);
        index += 1;
        break;
      case "--graph": {
        const value = requireValue(rest, index, arg);
        if (value !== "default" && value !== "named") {
          throw new Error(`Invalid --graph value "${value}"`);
        }
        pending.graphMode = value;
        index += 1;
        break;
      }
      case "--graph-iri":
        pending.graphIri = requireValue(rest, index, arg);
        index += 1;
        break;
      case "--method": {
        const value = requireValue(rest, index, arg).toUpperCase();
        if (value !== "PUT" && value !== "POST") {
          throw new Error(`Invalid --method value "${value}"`);
        }
        method = value;
        index += 1;
        break;
      }
      default:
        throw new Error(`Unknown argument: ${arg}\n\n${HELP_TEXT}`);
    }
  }

  const command = buildCommand(commandName, pending, method);
  return {
    baseUrl: pending.baseUrl,
    token: pending.token,
    headers: pending.headers,
    command,
  };
}

function buildCommand(
  commandName: string,
  pending: PendingOptions,
  method: "PUT" | "POST",
): CliCommand {
  switch (commandName) {
    case "runtime":
      return { kind: "runtime" };
    case "capabilities":
      return { kind: "capabilities" };
    case "reasoning":
      return { kind: "reasoning" };
    case "query":
      return { kind: "query", text: requireText(pending), accept: pending.accept };
    case "update":
      return { kind: "update", text: requireText(pending) };
    case "tell":
      validateGraphOptions(pending);
      return {
        kind: "tell",
        text: requireText(pending),
        graphMode: pending.graphMode,
        graphIri: pending.graphIri,
      };
    case "graph-read":
      validateGraphOptions(pending);
      return {
        kind: "graph-read",
        graphMode: pending.graphMode,
        graphIri: pending.graphIri,
      };
    case "graph-write":
      validateGraphOptions(pending);
      return {
        kind: "graph-write",
        method,
        text: requireText(pending),
        graphMode: pending.graphMode,
        graphIri: pending.graphIri,
      };
    case "graph-delete":
      validateGraphOptions(pending);
      return {
        kind: "graph-delete",
        graphMode: pending.graphMode,
        graphIri: pending.graphIri,
      };
    default:
      throw new Error(`Unknown command "${commandName}"\n\n${HELP_TEXT}`);
  }
}

function validateGraphOptions(pending: PendingOptions): void {
  if (pending.graphMode === "named" && !pending.graphIri) {
    throw new Error("Named graph operations require --graph-iri");
  }
}

function requireText(pending: PendingOptions): string {
  if (pending.text) {
    return pending.text;
  }
  if (pending.file) {
    return `@file:${pending.file}`;
  }
  throw new Error("Provide either --text or --file");
}
