# Plan: Multi-Route Containers with Per-Route Collection Schedules

> Note: This plan is **domain/architecture only**. The WasteHero platform code is not in this repo (`/home/user/vrp` is the generic VRP solver). The plan describes entity changes and workflow; the implementing repo will need to translate it into its ORM/services.

---

## Context

Today, every container in WasteHero has a **1:1 binding to a route scheme** and a **single collection calendar**. The container row directly stores:

- `route_scheme_id` (defines allowed waste fractions, vehicles, etc.)
- `collection_calendar` / pickup dates
- Pickup settings / frequency

This works for the standard case but breaks down when **the same physical container must be visited by more than one route**, with each route potentially using:

- a **different vehicle / operator** (e.g. side-loader on the main route, rear-loader for an overflow run);
- the **same fraction but a different schedule** (e.g. weekly main route + biweekly backup route);
- a **backup / overflow route** with its own irregular cadence.

There is no way to express "Container X is picked up by Route A on Mondays *and* by Route B once a month" without duplicating the container, which corrupts master data (one physical asset, two records).

The goal: let one `Container` participate in N routes, each with its own collection schedule, while keeping the container row as the single source of truth for the physical asset.

---

## Recommended Data Model

Introduce a **junction entity** between Container and Route, and **move scheduling off the Container** onto that junction (with the route still defining the master cadence).

### Entities

#### `Container` (existing — slimmed down)
Master record for the physical asset. **Remove** scheduling fields:
- ~~`route_scheme_id`~~ → derived via `ContainerRouteAssignment`
- ~~`collection_calendar`~~ / ~~`pickup_frequency`~~ → moved to assignment / route
- Keeps: location, type, capacity, owned-by, sensors, allowed-fractions (constraint, not assignment).

#### `Route` / `RouteScheme` (existing — extended)
- Owns the **master calendar** for the route (the dates the route is actually executed).
- Owns vehicle/operator/fraction constraints (already does today).
- New: `default_pickup_settings` (time window, service duration) used when an assignment doesn't override.

#### `ContainerRouteAssignment` (NEW — the junction)
The minimum-viable record connecting one container to one route. Fields:

| Field | Purpose |
|---|---|
| `container_id` | FK to Container |
| `route_id` | FK to Route |
| `role` | enum: `primary`, `secondary`, `backup`, `overflow` |
| `schedule_mode` | enum: `inherit_route`, `subset_of_route`, `custom_dates` |
| `date_filter` | nullable; when `subset_of_route`, e.g. "every 2nd execution", "first Monday of month", or a list of route-execution dates |
| `custom_dates` | nullable; only used when `schedule_mode = custom_dates` (ad-hoc / overflow) |
| `pickup_settings_override` | nullable; per-assignment time window / service duration / priority |
| `valid_from`, `valid_to` | activation window (seasonal routes, temporary backups) |
| `active` | soft on/off |

The **effective collection dates for a given (container, route)** are computed as:

```
inherit_route        → Route.calendar
subset_of_route      → Route.calendar ∩ date_filter
custom_dates         → custom_dates  (independent of Route.calendar)
```

A container's **overall** collection schedule is the **union** across all its active assignments.

### Why this shape

It directly answers your "calendar on route vs calendar on (container, route)" dilemma by making it **both, with a clear default**:

- The **route owns the master calendar** (single source of truth for "when does this route run"). This keeps planning sane: a dispatcher looks at one route, sees its dates, and the containers visited.
- The **assignment can refine** that calendar for an individual container — needed for backup/overflow and seasonal cases.
- 95% of assignments will use `schedule_mode = inherit_route` and stay simple. The complexity only appears where the business actually needs it.

### Why not "calendar lives only on the route"

- Backup and overflow routes by definition do not run on every regular route date for every container. Without an assignment-level filter you must invent one synthetic Route per cadence variation, which explodes the route catalog and makes route generation harder, not easier.
- Different operators for the same container at different times is naturally a different `Route` (different vehicle/skill), but their cadences rarely line up perfectly with the main route.

### Why not "calendar lives only on the assignment"

- Every assignment row would have to redundantly carry the master cadence; dispatchers couldn't ask "what's the schedule for Route A?" without aggregating across all assignments.
- Editing the route calendar (e.g. holiday shift) would require touching N assignments instead of one route.

---

## How a Container Gets Assigned to a Route

Assignment = creating a `ContainerRouteAssignment` row. Four entry points cover the realistic flows:

1. **From the Container detail page** — "Add to route" action: pick route, role, schedule mode (`inherit_route` / `subset_of_route` / `custom_dates`), optional pickup-settings override, optional valid_from/valid_to. Used for one-off attach.
2. **From the Route detail page** — "Add containers" multi-select (filter by zone, fraction, owner, etc.). Bulk-creates assignments with the same defaults (typically `role = primary`, `schedule_mode = inherit_route`). Used when building or extending a route.
3. **Rules-based / bulk import** — a saved rule like "every container in zone Z with fraction F belongs on Route A" auto-creates assignments when new containers are onboarded; CSV import for migrations or large customer onboardings. The rule writes the same `ContainerRouteAssignment` rows; nothing magic.
4. **API** — `POST /containers/{id}/route-assignments` and `POST /routes/{id}/container-assignments` for integrations / external dispatch systems.

Validation runs on every create/update regardless of entry point: container's allowed fractions ⊇ route's fraction; container's physical constraints (size, lift type) compatible with route's vehicle; `valid_from < valid_to`; `custom_dates` non-empty when chosen.

The container row itself is never edited by these flows — it stays the master record.

---

## Route Generation Workflow (after change)

For a given `Route` and a given `execution_date`:

1. Pull all `ContainerRouteAssignment` rows where `route_id = R`, `active = true`, and `valid_from ≤ execution_date ≤ valid_to`.
2. Filter to assignments whose **effective dates** include `execution_date`:
   - `inherit_route`: include if `execution_date ∈ Route.calendar`.
   - `subset_of_route`: also apply `date_filter`.
   - `custom_dates`: include if `execution_date ∈ custom_dates`.
3. For each surviving assignment, build a stop using `pickup_settings_override` if present, else `Route.default_pickup_settings`, else container defaults.
4. Hand the resulting stops to the VRP solver as jobs/services on that route's vehicles.

The solver itself does not need to know about multi-route containers — by the time stops reach it, the (container, route, date) triple is already resolved.

---

## Migration

1. **Add** `ContainerRouteAssignment` table.
2. **Backfill**: for every existing container, create one row with `route_id = container.route_scheme_id`, `schedule_mode = custom_dates`, `custom_dates = container.collection_calendar`, `role = primary`. (Use `custom_dates` rather than `inherit_route` so the migration is lossless even if a container's current dates don't match the route's calendar.)
3. **Switch reads**: route-generation, calendar views, and reporting now query through the assignment table.
4. **Deprecate** scheduling fields on `Container` (keep columns nullable for one release, then drop).
5. **Allow** new flow: users can attach a container to a second route and choose `inherit_route` / `subset_of_route` / `custom_dates`.

---

## UI / UX Implications

- Container detail page: replace single "Route & Calendar" panel with a **list of route assignments** (one row per route the container is on), each showing role, effective schedule preview, and edit affordance.
- Route detail page: container list now shows each container's `role` and any `subset_of_route` filter so dispatchers can spot "this is the one container that's only collected biweekly here".
- Calendar/agenda view for a container: render the **union** of all assignments' effective dates, color-coded per route.

---

## Edge Cases & Constraints

- **Conflicting fractions**: a container's allowed fractions must be a superset of every route it's assigned to. Validate on assignment create.
- **Same date on multiple routes (multiple same-day pickups)**: explicitly **allowed by default**. Each route execution is a discrete collection event and there are several real-world reasons to want more than one in a day:
  - High-volume locations (event venues, restaurants, markets) that fill faster than a single daily pickup.
  - Morning + evening pickups for businesses with shift-based waste generation.
  - A sensor-driven overflow / backup pickup on the same day as a regular scheduled one.
  - A re-run after a missed / incomplete primary pickup.

  The model treats each (container, route, execution_date) as its own collection event — no implicit dedupe across routes. Reporting and SLA calculations should aggregate events, not collapse them.

  An optional **per-route or per-assignment "single pickup per day" flag** can be offered for customers who explicitly want to prevent duplicate billing/collection on the same day; off by default.
- **Holiday shifts / one-off changes**: belong on `Route.calendar` (single edit), not on every assignment.
- **Decommissioning a container**: deactivate all its assignments, don't delete history.
- **Ad-hoc one-time pickup**: model as an assignment with `schedule_mode = custom_dates` and `valid_to` set to that date.

---

## Critical Files (to be created in the WasteHero repo, not this one)

- `Container` entity — strip scheduling fields.
- `Route` / `RouteScheme` entity — confirm calendar + default pickup settings live here.
- `ContainerRouteAssignment` entity — new.
- Route-generation service — switch source of truth from container fields to assignment table.
- Container detail / Route detail UI — multi-assignment views.
- Migration script — backfill + deprecation.

---

## Verification

End-to-end scenarios the new model must handle. Walk through each on paper / in a staging env before rollout:

1. **Status quo**: legacy container with one route, weekly calendar. After backfill, route generation must produce identical stops on identical dates.
2. **Backup route**: container on Route A (weekly, `inherit_route`) and Route B (`custom_dates` = first Monday of month). Generate a year of routes; container appears on Route B only on first Mondays, on Route A every week, and never on Route B's other execution dates.
   - Sub-case: when a first Monday IS also a Route A weekday, the container produces **two** collection events for that day (one on each route). The system must record both, not collapse them.
3. **Different operators**: container on Route A (rear-loader, `inherit_route`) and Route C (side-loader, `subset_of_route` = every 2nd execution). Solver receives correct vehicle constraint per route; no double-pickup on the same calendar day.
4. **Seasonal**: container on Route D with `valid_from`=Jun 1, `valid_to`=Aug 31. Outside the window the container does not appear on Route D's stop list.
5. **Conflict**: try assigning a fraction to a route that the container's allowed-fractions don't include — must be rejected at validation.
6. **Calendar edit**: shift Route A's calendar by one day for a holiday. All containers inheriting from Route A move with it; containers on `custom_dates` for Route A do not.
