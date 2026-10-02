# ADR-0008: Users, workspaces and graph policies in the store

Status: accepted (2026-10-02; the personal space is mandatory). The owner's direction: policies modifiable and justifiable at
the store level, but user control and the users' own space first; the best model for the
use cases ahead ([gap plan G2b](../plan/2026-10-02-engine-gaps.md)).

## Context

Graph-level access control (O1, delivered 2 October) reads a policy file: per role, the
graphs (IRIs, prefixes) it may read, write or is denied. It works and is enforced before
evaluation, but:

- only an operator with access to the server's files can change it, and a change needs a
  restart;
- nobody can see who changed what and why;
- a user has no space of their own: a researcher drafting statements before sharing them
  needs an administrator to write a rule for them;
- roles come only from tokens (OIDC, JWT, client certificates); a standalone or desktop
  installation has no users at all.

The use cases ahead: ResearchSpace (projects of researchers, drafts, review, publication),
the datamodel workflow (reviewed data written by a service account), research groups
sharing a server, and single users on a desktop.

How others do it: GraphDB grants per repository (read/write) and, since 10.x, per graph
through rules on roles; Stardog has users, roles and permissions on named graphs, managed
through its API and Studio; MarkLogic attaches permissions to documents. None of them
gives a user a space of their own; all of them keep the policy as server state that the
API and the UI manage, with an audit log.

## Decision

- **Workspaces are the user-facing unit.** A workspace is a named set of graphs (an IRI
  prefix of its own, `…/workspace/{name}/`, plus graphs it takes in) with members, each
  an owner, an editor or a viewer. Owners manage members; editors write the workspace's
  graphs; viewers read them. A project in ResearchSpace maps to a workspace.
- **Every user has a personal space:** a workspace only they own and write, readable by
  whoever they share it with. Drafts live there until they are moved (an update) into a
  shared workspace.
- **Roles stay for organisation-wide rules** (as the policy file has them today):
  readers of the public graphs, the datamodel workflow's service account, auditors. A
  user's rights are the union of its roles' and its memberships'; an explicit deny in a
  role still wins.
- **Policies are store state:** users, workspaces, memberships and role rules are kept
  as RDF in a system graph of the repository (readable to administrators, never to
  ordinary queries), changed only through the engine API, each change a commit with its
  author, time and a required reason; the history is queryable. The policy file stays as
  import and export.
- **Identity:** from tokens as today (OIDC, JWT, client certificates; claims map to
  users and roles), or local users (Argon2-hashed passwords in the system graph) for
  standalone and desktop installations. Administrators are users with the admin role.
- **Enforcement stays where it is:** the read and write sets of a request are computed
  from the identity once, and the query's dataset is restricted before evaluation; writes
  outside them are refused whole (as O1). Inferred statements follow the reasoner's
  provenance when it records it (G3), until then the `inferred` setting.

## Consequences

- The engine API gains users, workspaces, memberships, role rules and their history
  (`/api/v1/…`), and the frontend can manage them without file access.
- The system graph is part of backups and images, like the data.
- A policy change takes effect for the next request, without a restart; cached query
  results are keyed by the access they were computed for (already the case).
- ResearchSpace's own users and groups map onto these (by token claims or by
  provisioning through the API); the datamodel workflow gets a service account with a
  role.

## Implementation (2 October)

- The state and its rules are the core's (`nrese_store::access`): evaluation, who may change
  what, the RDF form (vocabulary `https://nrese.dev/ns/access#`) and the history. The
  server maps credentials to a principal and serves `/api/v1/access/…`
  ([HTTP interface](../ops/http-api.md#users-workspaces-and-policies-apiv1access)).
- The state is server-wide, kept in a system store of its own (`system/` of the data
  directory) rather than a graph of each repository: users and roles span repositories (a
  workspace names its repository, or applies to all), and a store of its own is out of
  reach of every query, update, reasoning and SHACL run on the data by construction. It
  has the data's durability (WAL, checkpoints).
- Enforcement is a setting of the state, off until a policy file is imported or an
  administrator turns it on: a server without a policy behaves as before.
- Personal spaces need no record: `~user` exists for every named user; a record appears
  when it is shared. Only administrators give a workspace graphs outside its prefix (else
  any user could take in any graph).
- Local users (Argon2 passwords) are the next step; provenance-based visibility of
  inferred statements waits for G3.
