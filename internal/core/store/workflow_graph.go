package store

import (
	"encoding/json"
	"fmt"

	"forge/internal/core/model"
)

// WorkflowGraph is the canonical workflow definition: typed nodes joined by
// conditional edges. A plain directive chain is the degenerate case; the
// general form adds script and switch nodes, failure/case edges, parallel
// fan-out with joins, and declared loop edges with iteration caps. The graph
// minus its loop edges must be acyclic, so evaluation always has a frontier.
type WorkflowGraph struct {
	Nodes []WorkflowNode      `json:"nodes" toml:"nodes"`
	Edges []WorkflowGraphEdge `json:"edges,omitempty" toml:"edges"`
}

// NodeType discriminates what a node does when it becomes ready.
type NodeType string

const (
	// NodeDirective runs one Work built from a directives-library file — the
	// content lives in git, the node supplies the binding.
	NodeDirective NodeType = "directive"
	// NodeScript runs an embedded JavaScript function in the daemon.
	NodeScript NodeType = "script"
	// NodeSwitch evaluates an expression and takes the matching case edge.
	NodeSwitch NodeType = "switch"
	// NodeJoin fans branches back in: all (default) or any.
	NodeJoin NodeType = "join"
)

// WorkflowNode is one node. Config is type-dependent and deliberately a map:
// it round-trips JSON (the editor) and TOML (the CLI) without loss, and the
// typed views below decode the fields the engine cares about.
type WorkflowNode struct {
	ID       string         `json:"id" toml:"id"`
	Type     NodeType       `json:"type" toml:"type"`
	Config   map[string]any `json:"config,omitempty" toml:"config,omitempty"`
	Position GraphPosition  `json:"position" toml:"position"`
}

// GraphPosition is where the editor drew the node. The engine ignores it; a
// graph whose positions are all zero is "unlaid" and the editor auto-lays it.
type GraphPosition struct {
	X float64 `json:"x" toml:"x"`
	Y float64 `json:"y" toml:"y"`
}

// EdgeWhen is the condition under which an edge is taken once its source node
// reaches a terminal state.
type EdgeWhen string

const (
	WhenSuccess EdgeWhen = "success" // source succeeded
	WhenFailure EdgeWhen = "failure" // source failed
	WhenAlways  EdgeWhen = "always"  // source finished either way
	WhenCase    EdgeWhen = "case"    // switch chose this edge's Case value
)

// WorkflowGraphEdge connects two nodes. A loop edge is the one sanctioned kind
// of cycle: it re-enters an earlier node until its cap, then goes dead.
type WorkflowGraphEdge struct {
	From          string   `json:"from" toml:"from"`
	To            string   `json:"to" toml:"to"`
	When          EdgeWhen `json:"when,omitempty" toml:"when,omitempty"`
	Case          string   `json:"case,omitempty" toml:"case,omitempty"`
	Default       bool     `json:"default,omitempty" toml:"default,omitempty"`
	StackOn       bool     `json:"stack_on,omitempty" toml:"stack_on,omitempty"`
	Loop          bool     `json:"loop,omitempty" toml:"loop,omitempty"`
	MaxIterations int      `json:"max_iterations,omitempty" toml:"max_iterations,omitempty"`
}

// Graph bounds. Script source is capped so a workflow row stays a
// definition, not a code repository.
const (
	MaxGraphNodes        = 100
	MaxGraphEdges        = 300
	MaxScriptSourceBytes = 32 * 1024
	MaxLoopIterations    = 20

	DefaultScriptTimeoutMS = 5_000
	MaxScriptTimeoutMS     = 30_000
)

// DirectiveNodeConfig is a directive node's typed view: which directive runs,
// with optional per-node overrides of the run's repositories/objective/persona
// and an operational envelope. Zero envelope values take the engine defaults.
type DirectiveNodeConfig struct {
	Directive    string   `json:"directive"`
	Repositories []string `json:"repositories,omitempty"`
	Objective    string   `json:"objective,omitempty"`
	// Persona overrides the directive's persona for this node's runs.
	Persona string `json:"persona,omitempty"`
	// Operational envelope.
	TimeoutSeconds int               `json:"timeout_seconds,omitempty"`
	MaxTurns       int               `json:"max_turns,omitempty"`
	BudgetClass    model.BudgetClass `json:"budget_class,omitempty"`
	Model          string            `json:"model,omitempty"`
	// Integrate queues the node's work (and any plan batch it spawns) for
	// merge onto the repo's integration branch — without it a review node's
	// repair tasks strand on branches (the cadence bug, 2026-09-06).
	Integrate bool `json:"integrate,omitempty"`
}

// ScriptNodeConfig is a script node's typed view: inline Source, or a named
// library script (scripts/<name>.js) — exactly one. Either way the code must
// define `function main(input)`; its JSON-serialized return value is the
// node output. Params rides into input.params for named scripts.
type ScriptNodeConfig struct {
	Source    string         `json:"source,omitempty"`
	Script    string         `json:"script,omitempty"`
	Params    map[string]any `json:"params,omitempty"`
	TimeoutMS int            `json:"timeout_ms,omitempty"`
}

// SwitchNodeConfig is a switch node's typed view: a JavaScript expression over
// `input` whose String()ed value selects the matching case edge.
type SwitchNodeConfig struct {
	Expression string `json:"expression"`
}

// JoinNodeConfig is a join node's typed view.
type JoinNodeConfig struct {
	Mode string `json:"mode,omitempty"` // "all" (default) | "any"
}

// nodeConfig decodes a node's config map into its typed view via JSON, so
// unknown keys (editor extensions, comments) survive storage untouched.
func nodeConfig[T any](n WorkflowNode) (T, error) {
	var out T
	raw, err := json.Marshal(n.Config)
	if err != nil {
		return out, fmt.Errorf("node %s: encode config: %w", n.ID, err)
	}
	if err := json.Unmarshal(raw, &out); err != nil {
		return out, fmt.Errorf("node %s: decode %s config: %w", n.ID, n.Type, err)
	}
	return out, nil
}

// DirectiveConfig, ScriptConfig, SwitchConfig, JoinConfig are the typed views;
// they error only on structurally wrong config (a string where a list goes).
func (n WorkflowNode) DirectiveConfig() (DirectiveNodeConfig, error) {
	return nodeConfig[DirectiveNodeConfig](n)
}
func (n WorkflowNode) ScriptConfig() (ScriptNodeConfig, error) {
	return nodeConfig[ScriptNodeConfig](n)
}
func (n WorkflowNode) SwitchConfig() (SwitchNodeConfig, error) {
	return nodeConfig[SwitchNodeConfig](n)
}
func (n WorkflowNode) JoinConfig() (JoinNodeConfig, error) { return nodeConfig[JoinNodeConfig](n) }

// Node returns the node by id, or nil.
func (g *WorkflowGraph) Node(id string) *WorkflowNode {
	for i := range g.Nodes {
		if g.Nodes[i].ID == id {
			return &g.Nodes[i]
		}
	}
	return nil
}

// Validate is what the API and CLI reject on before anything is stored. The
// invariant everything downstream leans on: the graph minus loop edges is a
// DAG with at least one root, every edge condition is legal for its source
// node's type, and every loop edge carries an iteration cap.
func (g *WorkflowGraph) Validate() error {
	if len(g.Nodes) == 0 {
		return fmt.Errorf("at least one node is required")
	}
	if len(g.Nodes) > MaxGraphNodes {
		return fmt.Errorf("%d nodes exceed %d", len(g.Nodes), MaxGraphNodes)
	}
	if len(g.Edges) > MaxGraphEdges {
		return fmt.Errorf("%d edges exceed %d", len(g.Edges), MaxGraphEdges)
	}
	types := map[string]NodeType{}
	for _, n := range g.Nodes {
		if err := model.ValidateName(n.ID); err != nil {
			return fmt.Errorf("node id: %w", err)
		}
		if _, dup := types[n.ID]; dup {
			return fmt.Errorf("node %s listed twice", n.ID)
		}
		types[n.ID] = n.Type
		if err := g.validateNodeConfig(n); err != nil {
			return err
		}
	}
	seen := map[[4]string]bool{}
	for _, e := range g.Edges {
		fromType, ok := types[e.From]
		if !ok {
			return fmt.Errorf("edge %s→%s: unknown node %q", e.From, e.To, e.From)
		}
		if _, ok := types[e.To]; !ok {
			return fmt.Errorf("edge %s→%s: unknown node %q", e.From, e.To, e.To)
		}
		key := [4]string{e.From, e.To, string(e.When), e.Case}
		if seen[key] {
			return fmt.Errorf("edge %s→%s (%s %s) listed twice", e.From, e.To, e.When, e.Case)
		}
		seen[key] = true
		if err := validateEdge(e, fromType); err != nil {
			return err
		}
	}
	if err := g.validateSwitchArms(types); err != nil {
		return err
	}
	if err := g.validateJoins(types); err != nil {
		return err
	}
	if _, err := g.TopoOrder(); err != nil {
		return err
	}
	roots := 0
	incoming := map[string]bool{}
	for _, e := range g.Edges {
		if !e.Loop {
			incoming[e.To] = true
		}
	}
	for _, n := range g.Nodes {
		if !incoming[n.ID] {
			roots++
		}
	}
	if roots == 0 {
		return fmt.Errorf("no root node: every node has a non-loop incoming edge")
	}
	return nil
}

func (g *WorkflowGraph) validateNodeConfig(n WorkflowNode) error {
	switch n.Type {
	case NodeDirective:
		cfg, err := n.DirectiveConfig()
		if err != nil {
			return err
		}
		if _, _, err := ParseTarget("directive:" + cfg.Directive); err != nil {
			return fmt.Errorf("node %s: %w", n.ID, err)
		}
		if cfg.TimeoutSeconds < 0 || cfg.TimeoutSeconds > 8*3600 {
			return fmt.Errorf("node %s: timeout_seconds %d: want 0..28800", n.ID, cfg.TimeoutSeconds)
		}
		if cfg.MaxTurns < 0 {
			return fmt.Errorf("node %s: max_turns must not be negative", n.ID)
		}
		if cfg.BudgetClass != "" && !cfg.BudgetClass.Valid() {
			return fmt.Errorf("node %s: budget_class %q", n.ID, cfg.BudgetClass)
		}
	case NodeScript:
		cfg, err := n.ScriptConfig()
		if err != nil {
			return err
		}
		if (cfg.Source == "") == (cfg.Script == "") {
			return fmt.Errorf("node %s: exactly one of source (inline) or script (a scripts/ library name) is required", n.ID)
		}
		if cfg.Script != "" {
			if _, _, err := ParseTarget("script:" + cfg.Script); err != nil {
				return fmt.Errorf("node %s: %w", n.ID, err)
			}
		}
		if len(cfg.Source) > MaxScriptSourceBytes {
			return fmt.Errorf("node %s: script source %d bytes exceeds %d", n.ID, len(cfg.Source), MaxScriptSourceBytes)
		}
		if cfg.TimeoutMS < 0 || cfg.TimeoutMS > MaxScriptTimeoutMS {
			return fmt.Errorf("node %s: timeout_ms %d: want 0..%d", n.ID, cfg.TimeoutMS, MaxScriptTimeoutMS)
		}
	case NodeSwitch:
		cfg, err := n.SwitchConfig()
		if err != nil {
			return err
		}
		if cfg.Expression == "" {
			return fmt.Errorf("node %s: switch expression is required", n.ID)
		}
	case NodeJoin:
		cfg, err := n.JoinConfig()
		if err != nil {
			return err
		}
		switch cfg.Mode {
		case "", "all", "any":
		default:
			return fmt.Errorf("node %s: join mode %q: want all or any", n.ID, cfg.Mode)
		}
	default:
		return fmt.Errorf("node %s: unknown type %q", n.ID, n.Type)
	}
	return nil
}

func validateEdge(e WorkflowGraphEdge, fromType NodeType) error {
	if e.Loop {
		if e.MaxIterations < 1 || e.MaxIterations > MaxLoopIterations {
			return fmt.Errorf("loop edge %s→%s: max_iterations %d: want 1..%d", e.From, e.To, e.MaxIterations, MaxLoopIterations)
		}
	} else if e.MaxIterations != 0 {
		return fmt.Errorf("edge %s→%s: max_iterations only applies to loop edges", e.From, e.To)
	}
	isCase := e.When == WhenCase || e.Default
	if fromType == NodeSwitch {
		if !isCase && e.When != WhenFailure {
			return fmt.Errorf("edge %s→%s: a switch's edges are case, default, or failure, not %q", e.From, e.To, e.When)
		}
		if e.When == WhenCase && e.Case == "" && !e.Default {
			return fmt.Errorf("edge %s→%s: case edge needs a case value or default", e.From, e.To)
		}
	} else {
		if isCase {
			return fmt.Errorf("edge %s→%s: case edges may only leave a switch node", e.From, e.To)
		}
		switch e.When {
		case "", WhenSuccess, WhenFailure, WhenAlways:
		default:
			return fmt.Errorf("edge %s→%s: when %q: want success, failure, or always", e.From, e.To, e.When)
		}
		if fromType == NodeJoin && e.When == WhenFailure {
			return fmt.Errorf("edge %s→%s: a join cannot fail; it succeeds or is skipped", e.From, e.To)
		}
	}
	if e.StackOn && (e.When != "" && e.When != WhenSuccess) {
		return fmt.Errorf("edge %s→%s: stack_on requires a success edge", e.From, e.To)
	}
	return nil
}

// validateSwitchArms checks each switch's outgoing set: at least one case,
// at most one default, no duplicate case values.
func (g *WorkflowGraph) validateSwitchArms(types map[string]NodeType) error {
	for _, n := range g.Nodes {
		if n.Type != NodeSwitch {
			continue
		}
		cases, defaults := 0, 0
		values := map[string]bool{}
		for _, e := range g.Edges {
			if e.From != n.ID {
				continue
			}
			if e.Default {
				defaults++
			} else if e.When == WhenCase {
				cases++
				if values[e.Case] {
					return fmt.Errorf("switch %s: case %q listed twice", n.ID, e.Case)
				}
				values[e.Case] = true
			}
		}
		if cases == 0 && defaults == 0 {
			return fmt.Errorf("switch %s has no case edges", n.ID)
		}
		if defaults > 1 {
			return fmt.Errorf("switch %s has %d default edges; one at most", n.ID, defaults)
		}
	}
	return nil
}

// validateJoins requires ≥ 2 non-loop incoming edges: a one-armed join is a
// drawing mistake, not a fan-in.
func (g *WorkflowGraph) validateJoins(types map[string]NodeType) error {
	for _, n := range g.Nodes {
		if n.Type != NodeJoin {
			continue
		}
		in := 0
		for _, e := range g.Edges {
			if e.To == n.ID && !e.Loop {
				in++
			}
		}
		if in < 2 {
			return fmt.Errorf("join %s has %d incoming edge(s); a join fans in at least 2", n.ID, in)
		}
	}
	return nil
}

// TopoOrder returns node ids in a topological order of the graph minus its
// loop edges, or an error naming a node on an undeclared cycle. This is both
// the validation of acyclicity and the instantiation order for run-now.
func (g *WorkflowGraph) TopoOrder() ([]string, error) {
	indegree := map[string]int{}
	next := map[string][]string{}
	for _, n := range g.Nodes {
		indegree[n.ID] = 0
	}
	for _, e := range g.Edges {
		if e.Loop {
			continue
		}
		indegree[e.To]++
		next[e.From] = append(next[e.From], e.To)
	}
	// Seed in declaration order so the order is stable for equal ranks.
	var queue []string
	for _, n := range g.Nodes {
		if indegree[n.ID] == 0 {
			queue = append(queue, n.ID)
		}
	}
	var order []string
	for len(queue) > 0 {
		id := queue[0]
		queue = queue[1:]
		order = append(order, id)
		for _, to := range next[id] {
			if indegree[to]--; indegree[to] == 0 {
				queue = append(queue, to)
			}
		}
	}
	if len(order) != len(g.Nodes) {
		for _, n := range g.Nodes {
			if indegree[n.ID] > 0 {
				return nil, fmt.Errorf("cycle through node %s: mark intentional back-edges loop = true with max_iterations", n.ID)
			}
		}
	}
	return order, nil
}
