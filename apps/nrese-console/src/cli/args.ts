import { DEFAULT_GRAPH, type GraphMode, type GraphTarget } from "../lib/endpoints";

export type CliCommand =
  | { kind: "runtime" }
  | { kind: "capabilities" }
  | { kind: "reasoning" }
  | { kind: "query"; text: string; accept: string }
  | { kind: "update"; text: string }
  | { kind: "tell"; text: string; target: GraphTarget }
  | { kind: "graph-read"; target: GraphTarget }
  | { kind: "graph-write"; method: "PUT" | "POST"; text: string; target: GraphTarget }
  | { kind: "graph-delete"; target: GraphTarget };

export type CliOptions = {
  baseUrl: string;
  token?: string;
  headers: Record<string, string>;
  /** Fail (exit 3) on answers the server doesn't vouch for as complete. */
  requireComplete: boolean;
  command: CliCommand;
};

type PendingOptions = {
  baseUrl: string;
  token?: string;
  headers: Record<string, string>;
  text?: string;
  file?: string;
  accept: string;
  /** As given; unset when the flag wasn't. */
  graphMode?: GraphMode;
  graphIri?: string;
};

const HELP_TEXT = `Usage:
  npm run cli -- runtime
  npm run cli -- capabilities
  npm run cli -- reasoning
  npm run cli -- query --text "SELECT * WHERE { ?s ?p ?o } LIMIT 5"
  npm run cli -- update --file .\\change.ru
  npm run cli -- tell --file .\\data.ttl --graph-iri https://example.org/g
  npm run cli -- graph-read --graph-iri https://example.org/g
  npm run cli -- graph-write --method PUT --file .\\data.ttl --graph-iri https://example.org/g
  npm run cli -- graph-delete --graph-iri https://example.org/g
  npm run cli -- graph-delete --graph default

Global options:
  --base-url <url>     Backend base URL. Defaults to NRESE_API_BASE_URL or http://127.0.0.1:8080
  --token <token>      Bearer token. Defaults to NRESE_API_TOKEN
  --header <k:v>       Additional header. May be repeated.

Content options:
  --text <text>        Inline SPARQL/RDF payload
  --file <path>        Read SPARQL/RDF payload from file
  --accept <type>      Query accept header. Default: application/sparql-results+json
  --graph <mode>       default | named
  --graph-iri <iri>    The named graph to act on
                       graph-delete and graph-write --method PUT need --graph default or --graph-iri;
                       the others act on the default graph without either
  --method <verb>      PUT | POST for graph-write
  --require-complete   Exit 3 when the answers aren't shown to be complete (NRESE-Completeness)`;

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
  };

  let method: "PUT" | "POST" = "PUT";
  let requireComplete = false;

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
        if (pending.graphMode !== undefined) {
          throw new Error("--graph given twice: name one graph");
        }
        pending.graphMode = value;
        index += 1;
        break;
      }
      case "--graph-iri":
        if (pending.graphIri !== undefined) {
          throw new Error("--graph-iri given twice: name one graph");
        }
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
      case "--require-complete":
        requireComplete = true;
        break;
      default:
        throw new Error(`Unknown argument: ${arg}\n\n${HELP_TEXT}`);
    }
  }

  const command = buildCommand(commandName, pending, method);
  return {
    baseUrl: pending.baseUrl,
    token: pending.token,
    headers: pending.headers,
    requireComplete,
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
      refuseGraphFlags(commandName, pending);
      return { kind: "runtime" };
    case "capabilities":
      refuseGraphFlags(commandName, pending);
      return { kind: "capabilities" };
    case "reasoning":
      refuseGraphFlags(commandName, pending);
      return { kind: "reasoning" };
    case "query":
      refuseGraphFlags(commandName, pending);
      return { kind: "query", text: requireText(pending), accept: pending.accept };
    case "update":
      refuseGraphFlags(commandName, pending);
      return { kind: "update", text: requireText(pending) };
    case "tell":
      return { kind: "tell", text: requireText(pending), target: graphTarget(commandName, pending, false) };
    case "graph-read":
      return { kind: "graph-read", target: graphTarget(commandName, pending, false) };
    case "graph-write":
      return {
        kind: "graph-write",
        method,
        text: requireText(pending),
        target: graphTarget(commandName, pending, method === "PUT"),
      };
    case "graph-delete":
      return { kind: "graph-delete", target: graphTarget(commandName, pending, true) };
    default:
      throw new Error(`Unknown command "${commandName}"\n\n${HELP_TEXT}`);
  }
}

/** The one graph the flags name, or a refusal before any request (SYN-72): `--graph-iri`
 * names that graph, `--graph default` the default graph, and the two together contradict.
 * A command that drops or replaces a graph acts only on one named explicitly; one that adds
 * or reads takes the default graph when neither flag is given. */
function graphTarget(
  commandName: string,
  pending: PendingOptions,
  dropsOrReplaces: boolean,
): GraphTarget {
  const { graphMode, graphIri } = pending;
  if (graphMode === "default" && graphIri !== undefined) {
    throw new Error("--graph default and --graph-iri name different graphs: give one");
  }
  if (graphIri !== undefined) {
    return { kind: "named", iri: graphIri };
  }
  if (graphMode === "named") {
    throw new Error("Named graph operations require --graph-iri");
  }
  if (graphMode === undefined && dropsOrReplaces) {
    throw new Error(
      `${commandName} drops or replaces a graph: name it with --graph default or --graph-iri <iri>`,
    );
  }
  return DEFAULT_GRAPH;
}

function refuseGraphFlags(commandName: string, pending: PendingOptions): void {
  if (pending.graphMode !== undefined || pending.graphIri !== undefined) {
    throw new Error(`${commandName} takes no graph: --graph and --graph-iri don't apply`);
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
