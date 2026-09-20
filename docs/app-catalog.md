# App Catalog & Apps

The catalog turns a Helm chart + a JSON Schema into an installable app with a
schema-driven form in the UI.

## Catalog composition

`catalog.Catalog` loads, in order:

1. `builtin.go` — Plex, Nextcloud, Jellyfin.
2. `builtin_extra.go` — Home Assistant, Pi-hole, Gitea, Syncthing, Immich.
3. Any `*.json` files in the configured catalog path (if non-empty).

External JSON entries **override** built-ins of the same `name`.

## Entry shape

| Field | Meaning |
| --- | --- |
| `name` | Unique chart/release name |
| `displayName`, `description`, `icon` | UI presentation |
| `category` | media / productivity / networking / smart-home / development |
| `chart`, `repository` | Helm chart reference + repo |
| `version` | Curated app version |
| `schema` | JSON Schema for the install form (see below) |
| `defaultValues` | Merged with user values at install |
| `ports`, `website`, `tags` | Presentation / metadata |

### Example (Jellyfin, abridged)

```json
{
  "name": "jellyfin",
  "displayName": "Jellyfin",
  "category": "media",
  "chart": "jellyfin/jellyfin",
  "schema": {
    "type": "object",
    "properties": {
      "timezone": { "type": "string", "default": "UTC" },
      "mediaPath": { "type": "string", "default": "/media" }
    }
  },
  "defaultValues": { "timezone": "UTC", "mediaPath": "/media" }
}
```

## Install / manage lifecycle (Helm SDK)

| Action | API | Behavior |
| --- | --- | --- |
| Install | `POST /api/apps {name, values}` | catalog defaults merged (request wins) → `helm install` into `naslos` ns |
| List | `GET /api/apps` | Releases with status: `running` / `failed` / `pending` / `stopped` / `superseded` |
| Detail | `GET /api/apps/{name}` | Plus `values` |
| Reconfigure | `PUT /api/apps/{name} {values}` | `helm upgrade` |
| Uninstall | `DELETE /api/apps/{name}` | `helm uninstall` |
| Start/stop | *(stub)* | Scale via `replicaCount` value / Helm hooks |

PVC persistence: because apps keep their Helm releases and PVCs, scaling
replicas to 0 and back preserves data (this is the intended "stop" path).

The Helm client uses an in-process cache dir (`$TMP/naslos-helm-cache`) and a
chart repository file, so each API pod resolves charts independently.

## Schema-driven forms

`GET /api/catalog/{name}` returns the whole entry including `schema`
(JSON Schema). The UI renders `SchemaForm` from it:

| Schema feature | UI control |
| --- | --- |
| `type: string` | input |
| `format: password` | password input |
| `format: email` | email input |
| `type: boolean` | toggle |
| `type: object` (nested) | section / nested form |
| `required` | marked required |
| `default` | prefilled |

## Adding an app

1. Add a `builtin*.go` entry (or a JSON file in the catalog dir).
2. Define `schema` + `defaultValues`; install test in a dev cluster
   (`make dev-cluster`, `helm install` the chart manually first).
3. See [development.md](development.md#extending--add-a-catalog-app) for a checklist.

## Storage for apps

Apps use `local-path-provisioner`, the only provisioner installed; the
`naslos-zfs` ZFS LocalPV storage class was removed (AUDIT-M13) — see
[storage-zfs.md](storage-zfs.md#storage-classes--app-data).