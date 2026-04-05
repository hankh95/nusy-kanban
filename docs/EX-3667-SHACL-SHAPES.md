# nusy-kanban OWL/SHACL Shapes Reference

> EX-3667 | Source: `crates/nusy-kanban/ontology/`

All shapes are Turtle (`.ttl`) files shipped inside the nusy-kanban binary.
Project-local overrides live at `.yurtle-kanban/kanban.ttl`.

## 1. Namespace and Prefix Reference

| Prefix | Namespace | Used By |
|--------|-----------|---------|
| `kb:` | `https://nusy.dev/kanban/` | All item types, core properties |
| `sh:` | `http://www.w3.org/ns/shacl#` | SHACL vocabulary |
| `owl:` | `http://www.w3.org/2002/07/owl#` | OWL ontology declaration |
| `rdfs:` | `http://www.w3.org/2000/01/rdf-schema#` | Labels, comments, subclass relations |
| `xsd:` | `http://www.w3.org/2001/XMLSchema#` | Datatypes (dateTime, boolean, integer) |
| `hyp:` | `https://nusy.dev/hypothesis/` | Hypothesis RDF predicates |
| `paper:` | `https://nusy.dev/paper/` | Paper RDF predicates |
| `expr:` | `https://nusy.dev/experiment/` | Experiment RDF predicates |
| `measure:` | `https://nusy.dev/measure/` | Measure RDF predicates |
| `lit:` | `https://nusy.dev/literature/` | Literature RDF predicates |

### Core Datatype Properties (all items)

| Predicate | Range | Notes |
|-----------|-------|-------|
| `kb:id` | xsd:string | Type-specific ID pattern (required) |
| `kb:status` | xsd:string | Type-specific valid values (required) |
| `kb:priority` | xsd:string | low / medium / high / critical (required, defaults per type) |
| `kb:assignee` | xsd:string | M5 / DGX / Mini / unassigned (required on most dev types) |
| `kb:body` | xsd:string | Markdown body (required) |
| `kb:tags` | xsd:string | Comma-separated tags (optional) |
| `kb:updated_at` | xsd:dateTime | Auto-set on any mutation (optional) |
| `kb:priority_rank` | xsd:integer | Numeric sort key (optional, EX-3244) |
| `kb:resolution` | xsd:string | completed / superseded / wont_do / duplicate / obsolete / merged |
| `kb:closedBy` | xsd:string | PROP-XXX or agent name that closed the item |

### Core Object Properties (relations)

| Predicate | Domain | Range | Notes |
|-----------|--------|-------|-------|
| `kb:dependsOn` | kb:Item | kb:Item | Blocking dependency |
| `kb:related` | kb:Item | kb:Item | Informational link |
| `kb:implements` | kb:Expedition | kb:Voyage | Expedition delivers toward voyage |
| `kb:spawns` | kb:Voyage | kb:Expedition | Voyage creates expedition |
| `kb:tests` | kb:Hypothesis | kb:Paper | Hypothesis tests paper claims |
| `kb:validates` | kb:Experiment | kb:Hypothesis | Experiment validates hypothesis |
| `kb:measures` | kb:Measure | kb:Experiment | Measure tracks experiment data |

### Annotation Properties (template system)

| Predicate | Range | Notes |
|-----------|-------|-------|
| `kb:sectionName` | xsd:string | Section heading text |
| `kb:headingLevel` | xsd:integer | Markdown heading level (2 = `##`) |
| `kb:templateHint` | xsd:string | Placeholder content shown in template |
| `kb:required` | xsd:boolean | Whether section is mandatory |
| `kb:requiredSection` | sh:Node | Container for section metadata |
| `kb:sectionOrder` | rdf:List | Ordered list of section names |
| `kb:hasComment` | sh:Node | Reference to CommentShape |
| `kb:TurtleBlockShape` | sh:Node | Container for turtle block requirements |
| `kb:requiredPrefix` | xsd:string | Required prefix in turtle block |
| `kb:requiredPredicate` | rdf:Property | Required predicate in turtle block |

---

## 2. Item Type Shapes

### 2.1 Dev Board Types

#### Expedition (`kb:Expedition`)

**File:** `shapes/dev/expedition.ttl`

| Field | Constraint | Default |
|-------|-----------|---------|
| `kb:id` | Pattern `^EX-\d{4,}$` | required |
| `kb:priority` | In `low, medium, high, critical` | `medium` |
| `kb:assignee` | In `M5, DGX, Mini, unassigned` | `unassigned` |
| `kb:tags` | minCount 0 | — |
| `kb:dependsOn` | minCount 0 | — |
| `kb:related` | minCount 0 | — |
| `kb:updated_at` | xsd:dateTime, minCount 0 | — |
| `kb:priority_rank` | xsd:integer, minCount 0 | — |
| `kb:body` | minCount 1 | required |

**Required body sections (in order):**

1. `## V12/V13 Parity Check` (required)
2. `## Context` (required)
3. `## Phase 1: <Name>` (required, repeatable)
4. `## Tests` (required)
5. `## Definition of Done` (required)
6. `## Constraints` (required)

**Status:** `backlog | planning | ready | in_progress | review | done`

**V12/V13 equivalent:** `.yurtle-kanban/templates/nautical/expedition.md`

---

#### Voyage (`kb:Voyage`)

**File:** `shapes/dev/voyage.ttl`

| Field | Constraint | Default |
|-------|-----------|---------|
| `kb:id` | Pattern `^VY-\d{4,}$` | required |
| `kb:priority` | In `low, medium, high, critical` | `high` |
| `kb:tags` | minCount 0 | — |
| `kb:related` | minCount 0 | — |
| `kb:updated_at` | xsd:dateTime, minCount 0 | — |
| `kb:body` | minCount 1 | required |

**Required body sections (in order):**

1. `## Problem` (required)
2. `## Goal` (required)
3. `## Expeditions` (required, markdown table)
4. `## Crates` (optional)
5. `## Done When` (required)
6. `## Related` (optional)

**Status:** `backlog | planning | ready | in_progress | review | done`

**V12/V13 equivalent:** `.yurtle-kanban/templates/nautical/voyage.md`

---

#### Chore (`kb:Chore`)

**File:** `shapes/dev/chore.ttl`

| Field | Constraint | Default |
|-------|-----------|---------|
| `kb:id` | Pattern `^CH-\d{4,}$` | required |
| `kb:priority` | In `low, medium, high` | `medium` |
| `kb:assignee` | In `M5, DGX, Mini, unassigned` | `unassigned` |
| `kb:tags` | minCount 0 | — |
| `kb:dependsOn` | minCount 0 | — |
| `kb:updated_at` | xsd:dateTime, minCount 0 | — |
| `kb:body` | minCount 1 | required |

**Required body sections (in order):**

1. `## Goal` (required)
2. `## Context` (required)
3. `## Work` (required)
4. `## DoD` (required)

**Status:** `backlog | planning | ready | in_progress | review | done`

**V12/V13 equivalent:** `.yurtle-kanban/templates/nautical/chore.md`

---

#### Hazard (`kb:Hazard`)

**File:** `shapes/dev/hazard.ttl`

| Field | Constraint | Default |
|-------|-----------|---------|
| `kb:id` | Pattern `^HZ-\d{4,}$` | required |
| `kb:priority` | In `low, medium, high, critical` | `high` |
| `kb:tags` | minCount 0 | — |
| `kb:related` | minCount 0 | — |
| `kb:updated_at` | xsd:dateTime, minCount 0 | — |
| `kb:body` | minCount 1 | required |

**Required body sections (in order):**

1. `## Risk Description` (required)
2. `## Impact Assessment` (required)
3. `## Mitigation Plan` (required)
4. `## Resolution` (optional)

**Status:** `backlog | planning | ready | in_progress | review | done`

**V12/V13 equivalent:** none (new in nusy-kanban)

---

#### Signal (`kb:Signal`)

**File:** `shapes/dev/signal.ttl`

| Field | Constraint | Default |
|-------|-----------|---------|
| `kb:id` | Pattern `^SG-\d{4,}$` | required |
| `kb:priority` | In `low, medium, high, critical` | `low` |
| `kb:tags` | minCount 0 | — |
| `kb:related` | minCount 0 | — |
| `kb:updated_at` | xsd:dateTime, minCount 0 | — |
| `kb:body` | minCount 1 | required |

**Required body sections (in order):**

1. `## Observation` (required)
2. `## Conditions` (required)
3. `## Raw Data` (optional)

**Status:** `backlog | done` (minimal — capture first, triage later)

**V12/V13 equivalent:** `.yurtle-kanban/templates/nautical/signal.md`

---

#### Feature (`kb:Feature`)

**File:** `shapes/dev/feature.ttl`

| Field | Constraint | Default |
|-------|-----------|---------|
| `kb:id` | Pattern `^FT-\d{4,}$` | required |
| `kb:priority` | In `low, medium, high, critical` | `medium` |
| `kb:assignee` | In `M5, DGX, Mini, unassigned` | `unassigned` |
| `kb:tags` | minCount 0 | — |
| `kb:dependsOn` | minCount 0 | — |
| `kb:related` | minCount 0 | — |
| `kb:updated_at` | xsd:dateTime, minCount 0 | — |
| `kb:body` | minCount 1 | required |

**Required body sections (in order):**

1. `## User Story` (required)
2. `## Acceptance Criteria` (required)
3. `## Implementation Notes` (optional)

**Status:** `backlog | planning | ready | in_progress | review | done`

**V12/V13 equivalent:** `.yurtle-kanban/templates/software/feature.md`

---

#### Comment (`kb:Comment`)

**File:** `shapes/dev/comment.ttl` (also in `shapes/workflow/comments.ttl`)

Stored in `CommentsTable` (Arrow RecordBatch, EX-3244).

| Field | Constraint | Notes |
|-------|-----------|-------|
| `kb:comment_id` | Pattern `^CMT-[A-Z]+-\d{4,}-\d{3,}$` | e.g. `CMT-EX-3218-001` |
| `kb:item_id` | minCount 1 | Item this comment belongs to |
| `kb:author` | In `M5, DGX, Mini, Captain` | required |
| `kb:body` | minCount 1 | Markdown text |
| `kb:created_at` | xsd:dateTime | Auto-set by `nk comment` |
| `kb:parent_comment_id` | Pattern `^CMT-...$`, minCount 0 | For threaded replies |
| `kb:resolved` | xsd:boolean, default `false` | Resolved comments block approval |

---

### 2.2 Research Board Types

#### Hypothesis (`kb:Hypothesis`)

**File:** `shapes/research/hypothesis.ttl`

| Field | Constraint | Notes |
|-------|-----------|-------|
| `kb:id` | Pattern `^H-\d{3,}$` | e.g. `H-015` |
| `kb:tests` | class `kb:Paper`, minCount 1 | Must link to paper |
| `kb:acfDimension` | In `AC1, AC2, AC3, AC4, AC5` | optional |
| `kb:tags` | minCount 0 | — |
| `kb:related` | minCount 0 | — |
| `kb:updated_at` | xsd:dateTime, minCount 0 | — |
| `kb:body` | minCount 1 | required |

**Required body sections (in order):**

1. `## Claim` (required)
2. `## Rationale` (required)
3. `## Variables` (required)
4. `## Experiments` (required, table)
5. `## Falsifiable By` (required)
6. `## ACF Connection` (optional)
7. `## Turtle Block` (required — hyp:claim, hyp:testedBy, hyp:acfDimension)

**Turtle block required prefixes/predicates:** `hyp:`, `kb:`; predicates: `hyp:claim`, `hyp:testedBy`, `hyp:acfDimension`

**Status:** `draft | active | retired` (never "complete" — validated per experiment and version)

**V12/V13 equivalent:** `.yurtle-kanban/templates/hdd/hypothesis.md`

---

#### Paper (`kb:Paper`)

**File:** `shapes/research/paper.ttl`

| Field | Constraint | Notes |
|-------|-----------|-------|
| `kb:id` | Pattern `^PAPER-\d{3,}$` | e.g. `PAPER-108` |
| `kb:tags` | minCount 0 | — |
| `kb:related` | minCount 0 | — |
| `kb:updated_at` | xsd:dateTime, minCount 0 | — |
| `kb:body` | minCount 1 | required |

**Required body sections (in order):**

1. `## Abstract` (required)
2. `## Hypotheses Tested` (required, table)
3. `## Key Experiments` (required, table)
4. `## Outline` (required)
5. `## Submission Target` (optional)
6. `## Turtle Block` (required — paper:title, paper:status, paper:hypotheses)

**Turtle block required prefixes/predicates:** `paper:`, `kb:`; predicates: `paper:title`, `paper:status`, `paper:hypotheses`

**Status:** `draft | outline | writing | review | complete | abandoned`

**V12/V13 equivalent:** `.yurtle-kanban/templates/hdd/paper.md`

---

#### Experiment (`kb:Experiment`)

**File:** `shapes/research/experiment.ttl`

| Field | Constraint | Notes |
|-------|-----------|-------|
| `kb:id` | Pattern `^EXPR-\d{3,}` | e.g. `EXPR-131.1` |
| `kb:validates` | class `kb:Hypothesis`, minCount 1 | Must link to hypothesis |
| `kb:tags` | minCount 0 | — |
| `kb:related` | minCount 0 | — |
| `kb:updated_at` | xsd:dateTime, minCount 0 | — |
| `kb:body` | minCount 1 | required |

**Required body sections (in order):**

1. `## Purpose` (required)
2. `## Method — Participants` (required)
3. `## Method — Materials` (required)
4. `## Method — Procedure` (required)
5. `## Method — Configuration` (required, table)
6. `## Analysis Plan` (required)
7. `## Expected Results` (required)
8. `## Falsifiable By` (required)
9. `## Data Location` (required, table)
10. `## Turtle Block` (required — expr:runStatus for DGX queue integration)

**Turtle block required prefixes/predicates:** `expr:`, `kb:`; predicates: `expr:hypothesis`, `expr:runStatus`, `expr:runOn`, `expr:requiresGPU`, `expr:estimatedMinutes`

**Status:** `planned | running | complete | abandoned` (one-shot, version-bound)

**V12/V13 equivalent:** `.yurtle-kanban/templates/hdd/experiment.md`

---

#### Measure (`kb:Measure`)

**File:** `shapes/research/measure.ttl`

| Field | Constraint | Notes |
|-------|-----------|-------|
| `kb:id` | Pattern `^M-\d{3,}$` | e.g. `M-098` |
| `kb:tags` | minCount 0 | — |
| `kb:related` | minCount 0 | — |
| `kb:updated_at` | xsd:dateTime, minCount 0 | — |
| `kb:body` | minCount 1 | required |

**Required body sections (in order):**

1. `## Description` (required)
2. `## Specification` (required, table: Unit, Category, Collection Method, Threshold)
3. `## Historical Values` (required, table)
4. `## Turtle Block` (required — measure:name, unit, category, collectionMethod)

**Turtle block required prefixes/predicates:** `measure:`, `kb:`; predicates: `measure:name`, `measure:unit`, `measure:category`, `measure:collectionMethod`

**Status:** `draft | active | retired` (ACF measures stay active indefinitely)

**V12/V13 equivalent:** `.yurtle-kanban/templates/hdd/measure.md`

---

#### Idea (`kb:Idea`)

**File:** `shapes/research/idea.ttl`

| Field | Constraint | Notes |
|-------|-----------|-------|
| `kb:id` | Pattern `^IDEA-\d{3,}$` | e.g. `IDEA-029` |
| `kb:tags` | minCount 0 | — |
| `kb:related` | minCount 0 | — |
| `kb:updated_at` | xsd:dateTime, minCount 0 | — |
| `kb:body` | minCount 1 | required |

**Required body sections (in order):**

1. `## Origin` (required)
2. `## Description` (required)
3. `## Domain` (required)
4. `## Next Steps` (required)

**No turtle block required.**

**Status:** `captured | formalized | abandoned` (lightweight — promoted to hypothesis or abandoned)

**V12/V13 equivalent:** none (ideas were informal in yurtle-kanban)

---

#### Literature (`kb:Literature`)

**File:** `shapes/research/literature.ttl`

| Field | Constraint | Notes |
|-------|-----------|-------|
| `kb:id` | Pattern `^LIT-\d{3,}$` | e.g. `LIT-001` |
| `kb:tags` | minCount 0 | — |
| `kb:related` | minCount 0 | — |
| `kb:updated_at` | xsd:dateTime, minCount 0 | — |
| `kb:body` | minCount 1 | required |

**Required body sections (in order):**

1. `## Topic` (required)
2. `## Search Strategy` (required)
3. `## Key Findings` (required)
4. `## Gaps` (required)
5. `## Framework Discovery` (optional)
6. `## Hypothesis Implications` (optional)
7. `## References` (required)
8. `## Turtle Block` (required — lit:topic, lit:searchDate)

**Turtle block required prefixes/predicates:** `lit:`, `kb:`; predicates: `lit:topic`, `lit:searchDate`

**Status:** `draft | active | complete`

**V12/V13 equivalent:** none (literature was ad-hoc in yurtle-kanban)

---

## 3. WIP Constraints (Board-Level)

**File:** `shapes/workflow/boards.ttl`

| Board | Constraint | SPARQL Logic |
|-------|-----------|--------------|
| Dev board (`kb:DevBoard`) | Max 4 items `in_progress` | `COUNT(?item) WHERE ?item kb:board "development" AND ?item kb:status "in_progress" > 4` |
| Research board (`kb:ResearchBoard`) | Max 5 items `active` | `COUNT(?item) WHERE ?item kb:board "research" AND ?item kb:status "active" > 5` |

These are **advisory in v1** — shown as warnings by `nk validate`, not hard blockers. They correspond to the `wip_limits` section in the legacy `.yurtle-kanban/config.yaml`.

---

## 4. Status Constraints Per Type

**File:** `shapes/workflow/states.ttl`

### Dev Board

| Type | Valid Statuses |
|------|---------------|
| Expedition, Voyage, Chore, Hazard, Feature | `backlog`, `planning`, `ready`, `in_progress`, `review`, `done` |
| Signal | `backlog`, `done` |

### Research Board

| Type | Valid Statuses |
|------|---------------|
| Hypothesis | `draft`, `active`, `retired` |
| Measure | `draft`, `active`, `retired` |
| Experiment | `planned`, `running`, `complete`, `abandoned` |
| Paper | `draft`, `outline`, `writing`, `review`, `complete`, `abandoned` |
| Idea | `captured`, `formalized`, `abandoned` |
| Literature | `draft`, `active`, `complete` |

---

## 5. Terminal State and Resolution Constraints

**File:** `shapes/workflow/terminal.ttl`

### Terminal Resolution Rule

Any item in a terminal state (`done`, `complete`, `abandoned`, `retired`) **must** have a `kb:resolution` set.

**Valid resolution values:** `completed | superseded | wont_do | duplicate | obsolete | merged`

```sparql
SELECT $this WHERE {
    $this kb:status ?s .
    FILTER(?s IN ("done", "complete", "abandoned", "retired"))
    FILTER NOT EXISTS { $this kb:resolution ?r }
}
```

### Closed-By Advisory

Items with a resolution **should** also have `kb:closedBy` set (PROP-XXX or agent name) for provenance.

---

## 6. Template Annotations: `kb:requiredSection` and `kb:sectionOrder`

The `nk templates` command reads these annotations to generate a pre-filled body skeleton.

### `kb:requiredSection` Structure

```turtle
kb:requiredSection [
    kb:sectionName "Section Title" ;
    sh:order 1 ;
    kb:headingLevel 2 ;
    kb:templateHint "Placeholder content shown in template." ;
    kb:required true
] .
```

| Field | Purpose |
|-------|---------|
| `kb:sectionName` | The ## heading text |
| `sh:order` | Display order within the item type |
| `kb:headingLevel` | Markdown heading level (2 = `##`) |
| `kb:templateHint` | Placeholder text inserted in the generated template |
| `kb:required` | Whether this section is mandatory (boolean) |
| `kb:repeatable` | Whether the section can appear multiple times (e.g. Phase N) |

### `kb:sectionOrder`

A fixed-ordered RDF list of section names that drives template generation:

```turtle
kb:sectionOrder ( "V12/V13 Parity Check" "Context" "Phases" "Tests" "Definition of Done" "Constraints" ) .
```

### Research Turtle Block Shapes

Five research types (Hypothesis, Paper, Experiment, Measure, Literature) have a `kb:TurtleBlockShape` node with `kb:requiredPrefix` and `kb:requiredPredicate` entries specifying what must appear in the embedded turtle block. The template hint provides the exact boilerplate to paste.

---

## 7. Relation Predicate Constraints

**File:** `shapes/workflow/relations.ttl`

### Generic (any type)

| Predicate | Shape | Range |
|-----------|-------|-------|
| `kb:dependsOn` | `kb:DependsOnShape` | `kb:Item` |
| `kb:related` | `kb:RelatedShape` | `kb:Item` |
| `kb:blocks` | `kb:BlocksShape` | `kb:Item` (inverse of dependsOn) |

### Typed (specific domain/range)

| Predicate | Domain | Range | Required? |
|-----------|--------|-------|-----------|
| `kb:implements` | `kb:Expedition` | `kb:Voyage` | no |
| `kb:spawns` | `kb:Voyage` | `kb:Expedition` | no |
| `kb:tests` | `kb:Hypothesis` | `kb:Paper` | **yes** |
| `kb:validates` | `kb:Experiment` | `kb:Hypothesis` | **yes** |
| `kb:measures` | `kb:Measure` | `kb:Experiment` | no |

---

## 8. Validation Example

### Python + rdflib (standalone validation)

```python
"""Validate a kanban item RDF graph against its SHACL shape."""
from rdflib import Graph, Namespace, URIRef
from rdflib.namespace import RDF

KB = Namespace("https://nusy.dev/kanban/")
SH = Namespace("http://www.w3.org/ns/shacl#")

# Load the item graph (e.g. loaded from Arrow, NATS, or a .ttl file)
item_graph = Graph()
item_graph.parse(data="""
    @prefix kb: <https://nusy.dev/kanban/> .
    @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

    <https://nusy.dev/kanban/item/EX-3218> a kb:Expedition ;
        kb:id "EX-3218" ;
        kb:priority "high" ;
        kb:assignee "Mini" ;
        kb:status "in_progress" ;
        kb:body "## Context\\n\\nTest context\\n\\n## V12/V13 Parity Check\\n\\n- none\\n\\n## Phase 1: Test\\n\\n- [ ] test\\n\\n## Tests\\n\\n- [ ] cargo test\\n\\n## Definition of Done\\n\\n- [ ] done\\n\\n## Constraints\\n\\n- Do NOT: over-engineer" .
""", format="turtle")

# Load the shapes graph
shapes_graph = Graph()
shapes_graph.parse("/Users/hankh19/Projects/nusy-product-team/crates/nusy-kanban/ontology/shapes/dev/expedition.ttl", format="turtle")

# Find the NodeShape in the shapes graph
for shape in shapes_graph.subjects(RDF.type, SH.NodeShape):
    target = next(shapes_graph.objects(shape, SH.targetClass), None)
    if target == KB.Expedition:
        print(f"Validating against shape: {shape}")
        # In production, use pyshacl to run the full SHACL engine:
        # from pyshacl import validate
        # conforms, results_graph, results_text = validate(item_graph, shacl_graph=shapes_graph)
        # print(results_text)
        break
```

### Apache Jena (command-line)

```bash
# Validate an expedition item against its shape using Apache Jena's SHACL API
java -jar Jena/bin/jena shacl \
  --shapes=crates/nusy-kanban/ontology/shapes/dev/expedition.ttl \
  --data=example-expedition.ttl \
  --report=TXT
```

### Expected violations

For an expedition with `kb:id "EXP-3218"` (wrong prefix, should be `EX-`) and missing body:

```
[Violation] kb:id pattern ^EX-\d{4,}$ — value "EXP-3218" does not match
[Violation] kb:body minCount 1 — property does not meet minCount constraint
```

---

## 9. File Index

```
crates/nusy-kanban/ontology/
  kanban.ttl                        # Core OWL ontology (classes + properties)
  shapes/
    dev/
      expedition.ttl                 # Expedition shape + section template
      chore.ttl                      # Chore shape + section template
      hazard.ttl                     # Hazard shape
      voyage.ttl                    # Voyage shape
      signal.ttl                    # Signal shape (minimal)
      feature.ttl                   # Feature shape
      groups.ttl                    # sh:PropertyGroup definitions
      comment.ttl                   # Comment shape
    research/
      hypothesis.ttl               # Hypothesis + turtle block shape
      paper.ttl                    # Paper + turtle block shape
      experiment.ttl               # Experiment + turtle block shape
      measure.ttl                  # Measure + turtle block shape
      idea.ttl                     # Idea shape (no turtle block)
      literature.ttl               # Literature + turtle block shape
      groups.ttl                   # Research PropertyGroup definitions
    workflow/
      boards.ttl                    # WIP limit SPARQL constraints
      states.ttl                   # Per-type valid-status shapes
      terminal.ttl                 # Terminal resolution rules
      relations.ttl                # Typed relation predicate constraints
      comments.ttl                 # Comment lifecycle shapes
      mutations.ttl               # updated_at auto-set invariant
```
