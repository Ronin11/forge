package config

import (
	"fmt"
	"sort"

	"github.com/BurntSushi/toml"
)

// M10 runners, models, and routing (DESIGN.md §21). The runner/model/routing
// tables ship as EMBEDDED defaults (defaultRunners, defaultModels,
// defaultRouting): bootstrap does not write them to config.toml, so a Forge
// upgrade refreshes the built-in prices and any config.toml only carries the
// operator's overrides. LoadConfig merges the embedded defaults under whatever
// the file provides — per key, with a zero field inheriting the embedded value
// (documented on the fields), and a wholly new runner/model added as-is.

// RunnerConfig is one place an attempt's agent can run: the local `claude`
// subscription, a first-party Anthropic key, or an OpenAI-compatible endpoint.
type RunnerConfig struct {
	// Kind selects the health probe and the executor shape.
	Kind string `toml:"kind"` // claude-cli | anthropic | openai-compatible
	// Billing is how spend is accounted: a subscription's notional cost is
	// tokens×price against the five-hour/seven-day windows; api and local are
	// real dollars.
	Billing string `toml:"billing"` // subscription | api | local
	// Capacity is the runner-slot count: at most this many attempts run on the
	// runner at once (a second scheduler dimension beside worker slots). Zero
	// means unbounded — bounded only by worker slots, which is what the `claude`
	// runner wants (every claude attempt already holds a worker slot).
	Capacity int `toml:"capacity"`
	// Endpoint is the OpenAI-compatible base URL (…/v1); empty for claude-cli.
	Endpoint string `toml:"endpoint"`
	// Probe overrides the health-probe path for openai-compatible runners;
	// empty uses the default /models, appended to the version-carrying
	// Endpoint above.
	Probe string `toml:"probe"`
	// DailyUSDCap and USDPerHour bound spend on an api/local runner; zero is no
	// cap. They are advisory to the operator (surfaced by doctor); the budget
	// policy still owns admission.
	DailyUSDCap float64 `toml:"daily_usd_cap"`
	USDPerHour  float64 `toml:"usd_per_hour"`
}

// Price is a model's list price in dollars per million tokens ($/MTok). A zero
// field inherits the embedded default for a built-in model, so an override can
// change one number without restating the table.
type Price struct {
	Input      float64 `toml:"input"`
	Output     float64 `toml:"output"`
	CacheRead  float64 `toml:"cache_read"`
	CacheWrite float64 `toml:"cache_write"`
}

// ModelConfig is one alias the router may choose: which runner runs it, its
// executor model id, its class and cost. Extends the M1 alias table (which
// only mapped alias→id) with the routing dimensions.
type ModelConfig struct {
	Runner   string `toml:"runner"`   // must name a [runners.<name>]
	ID       string `toml:"id"`       // the executor model id (claude --model)
	Class    string `toml:"class"`    // frontier | mid | small | local
	MaxTier  int    `toml:"max_tier"` // 0..3; a routine of tier T may use models with max_tier ≥ T
	Executor string `toml:"executor"` // executor name; empty inherits the routine's
	Context  int    `toml:"context"`  // context window in tokens; 0 unknown
	Price    Price  `toml:"price"`
}

// RoutingConfig tunes the router (DESIGN.md §21). Weights scale the cost-vector
// terms; the Wilson gate needs MinSamples before it trusts a model's verified
// success ≥ MinVerifiedSuccess; below MinSamples a model is eligible with
// probability Explore.
type RoutingConfig struct {
	Objective          string  `toml:"objective"` // documentation only: "min_cost" etc.
	Weights            Weights `toml:"weights"`
	MinVerifiedSuccess float64 `toml:"min_verified_success"`
	MinSamples         int     `toml:"min_samples"`
	Explore            float64 `toml:"explore"`
	// Ladder is the default retry-escalation ladder (weakest → strongest
	// alias). A retry after a failed attempt climbs one rung above the failed
	// attempt's model even when the routine declares no models allowlist —
	// evidence-triggered escalation, never a first-attempt default. Empty
	// disables. Routines with their own allowlist keep their explicit ladder.
	Ladder []string `toml:"ladder"`
	// BacklogCeiling is the highest ladder alias backlog-class work may reach
	// (inclusive); the top rungs cost real money and backlog work shouldn't
	// ride there unattended. Empty = the full ladder.
	BacklogCeiling string `toml:"backlog_ceiling"`
}

// Weights scales each cost-vector term in the router's score; all non-negative.
type Weights struct {
	USD           float64 `toml:"usd"`
	FiveHour      float64 `toml:"five_hour"`
	SevenDay      float64 `toml:"seven_day"`
	RunnerSeconds float64 `toml:"runner_seconds"`
}

// defaultRunners is the embedded runner table: only `claude`, the local
// subscription CLI. Capacity 0 means "bounded by worker slots" (§21).
func defaultRunners() map[string]RunnerConfig {
	return map[string]RunnerConfig{
		"claude": {Kind: "claude-cli", Billing: "subscription", Capacity: 0},
	}
}

// defaultModels is the embedded model table: the three Anthropic aliases M1
// knew plus fable, with runner, class, and current list prices ($/MTok).
// Sonnet is classed `mid` (the workhorse between small haiku and the frontier
// pair — see NOTES.md M10); opus and fable are `frontier`; haiku is `small`.
// max_tier 3 lets any routine tier reach any of them. IDs match
// defaultResolveModel (server.go).
// CacheWrite is the 1-hour-TTL rate (2× input): the claude-code executor
// caches at the 1h TTL, and pricing writes at the 5m rate (1.25× input) is
// what doctor's notional-vs-reported drift was measuring.
func defaultModels() map[string]ModelConfig {
	return map[string]ModelConfig{
		"haiku": {
			Runner: "claude", ID: "claude-haiku-4-5-20251001", Class: "small", MaxTier: 3,
			Context: 200_000, Price: Price{Input: 1.00, Output: 5.00, CacheRead: 0.10, CacheWrite: 2.00},
		},
		"sonnet": {
			Runner: "claude", ID: "claude-sonnet-4-5", Class: "mid", MaxTier: 3,
			Context: 200_000, Price: Price{Input: 3.00, Output: 15.00, CacheRead: 0.30, CacheWrite: 6.00},
		},
		"opus": {
			Runner: "claude", ID: "claude-opus-4-1", Class: "frontier", MaxTier: 3,
			Context: 200_000, Price: Price{Input: 15.00, Output: 75.00, CacheRead: 1.50, CacheWrite: 30.00},
		},
		"fable": {
			Runner: "claude", ID: "claude-fable-5", Class: "frontier", MaxTier: 3,
			Context: 200_000, Price: Price{Input: 10.00, Output: 50.00, CacheRead: 1.00, CacheWrite: 20.00},
		},
	}
}

// defaultRouting is the embedded routing policy: minimise notional dollars,
// weight the two subscription windows lightly, and require a handful of
// verified successes before trusting a model, with a small exploration rate.
func defaultRouting() RoutingConfig {
	return RoutingConfig{
		Objective:          "min_cost",
		Weights:            Weights{USD: 1.0, FiveHour: 0.0, SevenDay: 0.0, RunnerSeconds: 0.0},
		MinVerifiedSuccess: 0.6,
		MinSamples:         5,
		Explore:            0.1,
	}
}

// applyModelDefaults merges the embedded runner/model/routing defaults under
// whatever config.toml provided. It consults the decoder's MetaData so a field
// the operator explicitly set — even to a zero value (an `explore = 0`, a
// deliberately free `price.input = 0`) — is respected, while an unmentioned
// field inherits the embedded default. meta is the zero value when no file was
// read, in which case nothing is "defined" and every default applies.
func (c *Config) applyModelDefaults(meta toml.MetaData) {
	if c.Runners == nil {
		c.Runners = map[string]RunnerConfig{}
	}
	for name, d := range defaultRunners() {
		c.Runners[name] = mergeRunner(meta, name, d, c.Runners[name])
	}
	if c.Models == nil {
		c.Models = map[string]ModelConfig{}
	}
	for name, d := range defaultModels() {
		c.Models[name] = mergeModel(meta, name, d, c.Models[name])
	}
	c.Routing = mergeRouting(meta, defaultRouting(), c.Routing)
}

func ok[V any](m map[string]V, k string) bool { _, present := m[k]; return present }

// mergeRunner fills a runner's fields the operator did not set from the default.
func mergeRunner(meta toml.MetaData, name string, def, user RunnerConfig) RunnerConfig {
	set := func(field string) bool { return meta.IsDefined("runners", name, field) }
	if !set("kind") {
		user.Kind = def.Kind
	}
	if !set("billing") {
		user.Billing = def.Billing
	}
	if !set("capacity") {
		user.Capacity = def.Capacity
	}
	if !set("endpoint") {
		user.Endpoint = def.Endpoint
	}
	if !set("probe") {
		user.Probe = def.Probe
	}
	if !set("daily_usd_cap") {
		user.DailyUSDCap = def.DailyUSDCap
	}
	if !set("usd_per_hour") {
		user.USDPerHour = def.USDPerHour
	}
	return user
}

// mergeModel fills a model's unmentioned fields from the embedded default so an
// override may restate one price without losing runner/class/id (§21).
func mergeModel(meta toml.MetaData, name string, def, user ModelConfig) ModelConfig {
	set := func(field ...string) bool { return meta.IsDefined(append([]string{"models", name}, field...)...) }
	if !set("runner") {
		user.Runner = def.Runner
	}
	if !set("id") {
		user.ID = def.ID
	}
	if !set("class") {
		user.Class = def.Class
	}
	if !set("max_tier") {
		user.MaxTier = def.MaxTier
	}
	if !set("executor") {
		user.Executor = def.Executor
	}
	if !set("context") {
		user.Context = def.Context
	}
	if !set("price", "input") {
		user.Price.Input = def.Price.Input
	}
	if !set("price", "output") {
		user.Price.Output = def.Price.Output
	}
	if !set("price", "cache_read") {
		user.Price.CacheRead = def.Price.CacheRead
	}
	if !set("price", "cache_write") {
		user.Price.CacheWrite = def.Price.CacheWrite
	}
	return user
}

func mergeRouting(meta toml.MetaData, def, user RoutingConfig) RoutingConfig {
	set := func(field ...string) bool { return meta.IsDefined(append([]string{"routing"}, field...)...) }
	if !set("objective") {
		user.Objective = def.Objective
	}
	if !set("weights", "usd") {
		user.Weights.USD = def.Weights.USD
	}
	if !set("weights", "five_hour") {
		user.Weights.FiveHour = def.Weights.FiveHour
	}
	if !set("weights", "seven_day") {
		user.Weights.SevenDay = def.Weights.SevenDay
	}
	if !set("weights", "runner_seconds") {
		user.Weights.RunnerSeconds = def.Weights.RunnerSeconds
	}
	if !set("min_verified_success") {
		user.MinVerifiedSuccess = def.MinVerifiedSuccess
	}
	if !set("min_samples") {
		user.MinSamples = def.MinSamples
	}
	if !set("explore") {
		user.Explore = def.Explore
	}
	return user
}

// validModelClass and validRunnerKind/validBilling are the closed sets.
var (
	validModelClass = map[string]bool{"frontier": true, "mid": true, "small": true, "local": true}
	validRunnerKind = map[string]bool{"claude-cli": true, "anthropic": true, "openai-compatible": true}
	validRunnerBill = map[string]bool{"subscription": true, "api": true, "local": true}
)

// validateModels checks the runner/model/routing tables after the merge.
func (c *Config) validateModels() error {
	for name, r := range c.Runners {
		if !validRunnerKind[r.Kind] {
			return fmt.Errorf("[runners.%s] kind %q must be claude-cli|anthropic|openai-compatible", name, r.Kind)
		}
		if !validRunnerBill[r.Billing] {
			return fmt.Errorf("[runners.%s] billing %q must be subscription|api|local", name, r.Billing)
		}
		if r.Capacity < 0 {
			return fmt.Errorf("[runners.%s] capacity must not be negative", name)
		}
		if r.Kind == "openai-compatible" && r.Endpoint == "" {
			return fmt.Errorf("[runners.%s] openai-compatible runner needs an endpoint", name)
		}
		if r.DailyUSDCap < 0 || r.USDPerHour < 0 {
			return fmt.Errorf("[runners.%s] daily_usd_cap and usd_per_hour must not be negative", name)
		}
	}
	for name, m := range c.Models {
		if m.ID == "" {
			return fmt.Errorf("[models.%s] id is required", name)
		}
		if m.Runner == "" || !ok(c.Runners, m.Runner) {
			return fmt.Errorf("[models.%s] runner %q names no [runners.*]", name, m.Runner)
		}
		if !validModelClass[m.Class] {
			return fmt.Errorf("[models.%s] class %q must be frontier|mid|small|local", name, m.Class)
		}
		if m.MaxTier < 0 || m.MaxTier > 3 {
			return fmt.Errorf("[models.%s] max_tier must be in 0..3", name)
		}
		p := m.Price
		if p.Input < 0 || p.Output < 0 || p.CacheRead < 0 || p.CacheWrite < 0 {
			return fmt.Errorf("[models.%s] price must not be negative", name)
		}
	}
	rt := c.Routing
	for label, w := range map[string]float64{"usd": rt.Weights.USD, "five_hour": rt.Weights.FiveHour, "seven_day": rt.Weights.SevenDay, "runner_seconds": rt.Weights.RunnerSeconds} {
		if w < 0 {
			return fmt.Errorf("[routing.weights] %s must not be negative", label)
		}
	}
	if rt.MinVerifiedSuccess < 0 || rt.MinVerifiedSuccess > 1 {
		return fmt.Errorf("[routing] min_verified_success must be in 0..1")
	}
	if rt.MinSamples < 0 {
		return fmt.Errorf("[routing] min_samples must not be negative")
	}
	if rt.Explore < 0 || rt.Explore > 1 {
		return fmt.Errorf("[routing] explore must be in 0..1")
	}
	return nil
}

// ModelInfo is the resolved view of one alias the router and facts use: the
// runner it runs on and that runner's billing, plus the model's class and
// price. It is the seam that replaces M1's alias→id lookup (server.go).
type ModelInfo struct {
	Alias      string
	ID         string
	Runner     string
	RunnerKind string
	Billing    string
	Class      string
	MaxTier    int
	Executor   string
	Context    int
	Price      Price
}

// ModelInfoFor resolves an alias against the config, or reports !ok. A bare
// executor id (never registered as an alias) is passed through with empty
// routing fields so a routine pinned to a full model id still runs (mirrors
// defaultResolveModel's pass-through).
func (c *Config) ModelInfoFor(alias string) (ModelInfo, bool) {
	if m, present := c.Models[alias]; present {
		r := c.Runners[m.Runner]
		return ModelInfo{
			Alias: alias, ID: m.ID, Runner: m.Runner, RunnerKind: r.Kind, Billing: r.Billing,
			Class: m.Class, MaxTier: m.MaxTier, Executor: m.Executor, Context: m.Context, Price: m.Price,
		}, true
	}
	if id, isID := passthroughModelID(alias); isID {
		return ModelInfo{Alias: alias, ID: id}, true
	}
	return ModelInfo{}, false
}

// ResolveModel is the alias→id seam ServerOptions.ResolveModel wants, backed by
// the config's model table.
func (c *Config) ResolveModel(alias string) (string, bool) {
	info, ok := c.ModelInfoFor(alias)
	if !ok {
		return "", false
	}
	return info.ID, true
}

// ModelAliases lists configured aliases sorted, for the stats matrix and CLI.
func (c *Config) ModelAliases() []string {
	out := make([]string, 0, len(c.Models))
	for name := range c.Models {
		out = append(out, name)
	}
	sort.Strings(out)
	return out
}

// passthroughModelID accepts a bare executor id — one carrying a "-<digit>"
// segment, as defaultResolveModel did — and returns it unchanged.
func passthroughModelID(alias string) (string, bool) {
	for i := 0; i+1 < len(alias); i++ {
		if alias[i] == '-' && alias[i+1] >= '0' && alias[i+1] <= '9' {
			return alias, true
		}
	}
	return "", false
}
