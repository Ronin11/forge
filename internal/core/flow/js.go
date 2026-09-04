// The script sandbox. A script or switch node runs inside a fresh goja
// interpreter (pure Go, no cgo) with nothing installed beyond `input` and a
// capped console: goja exposes no I/O unless the embedder binds host objects,
// and none are bound — no require, no process, no filesystem, no network.
// The fences are a wall-clock interrupt (Interrupt aborts allocation loops
// too), a call-stack cap, source and output size caps, and the per-run script
// budget the driver enforces.
package flow

import (
	"encoding/json"
	"fmt"
	"time"

	"github.com/dop251/goja"

	"forge/internal/core/store"
)

// RunInfo is the run context a script sees as input.run.
type RunInfo struct {
	ID           string   `json:"id"`
	Workflow     string   `json:"workflow"`
	Objective    string   `json:"objective,omitempty"`
	Repositories []string `json:"repositories,omitempty"`
	Trigger      string   `json:"trigger"`
	Iteration    int      `json:"iteration"`
}

// StepInput is one upstream node's outcome as a script sees it in
// input.steps[id]: the latest instance's status and iteration, plus — for
// routine nodes — the Work's state, the result summary, and per-repository
// targets, with Output always the node's data payload (the agent's `output`
// object, a script's return value, a switch's {"case": ...}).
type StepInput struct {
	Status    string `json:"status"`
	Iteration int    `json:"iteration"`
	State     string `json:"state,omitempty"`
	Summary   string `json:"summary,omitempty"`
	Output    any    `json:"output,omitempty"`
	Targets   any    `json:"targets,omitempty"`
}

// ScriptInput is the one value a script receives.
type ScriptInput struct {
	Run   RunInfo              `json:"run"`
	Steps map[string]StepInput `json:"steps"`
	// Params is the free-form parameter channel: a named script node's
	// config params, or a forge_script_run tool call's input. Zero for plain
	// inline graph scripts.
	Params any `json:"params,omitempty"`
}

// CompileScript is the save-time syntax check: a script that does not parse
// is a 400 at PUT, not a runtime failure mid-run.
func CompileScript(source string) error {
	if _, err := goja.Compile("script", source, true); err != nil {
		return fmt.Errorf("script does not compile: %w", err)
	}
	return nil
}

// CompileSwitch is the save-time check for a switch expression, compiled
// exactly as EvalSwitch wraps it.
func CompileSwitch(expr string) error {
	if _, err := goja.Compile("switch", "(function(input){ return ("+expr+"\n); })", true); err != nil {
		return fmt.Errorf("switch expression does not compile: %w", err)
	}
	return nil
}

// ScriptTimeout resolves a node's timeout config.
func ScriptTimeout(ms int) time.Duration {
	if ms <= 0 {
		ms = store.DefaultScriptTimeoutMS
	}
	if ms > store.MaxScriptTimeoutMS {
		ms = store.MaxScriptTimeoutMS
	}
	return time.Duration(ms) * time.Millisecond
}

// RunScript executes `function main(input)` from source and returns the
// JSON-serialized return value.
func RunScript(source string, input ScriptInput, timeout time.Duration) (json.RawMessage, error) {
	vm, stop, err := newVM(input, timeout)
	if err != nil {
		return nil, err
	}
	defer stop()
	if _, err := vm.RunString(source); err != nil {
		return nil, scriptErr(err)
	}
	mainFn, ok := goja.AssertFunction(vm.Get("main"))
	if !ok {
		return nil, fmt.Errorf("script must define function main(input)")
	}
	val, err := mainFn(goja.Undefined(), vm.Get("input"))
	if err != nil {
		return nil, scriptErr(err)
	}
	if val == nil || goja.IsUndefined(val) || goja.IsNull(val) {
		return json.RawMessage("null"), nil
	}
	out, err := json.Marshal(val.Export())
	if err != nil {
		return nil, fmt.Errorf("script output is not JSON-serializable: %w", err)
	}
	if len(out) > store.MaxNodeOutputBytes {
		return nil, fmt.Errorf("script output %d bytes exceeds %d", len(out), store.MaxNodeOutputBytes)
	}
	return out, nil
}

// EvalSwitch evaluates a switch expression over input and returns its
// String()ed value — what the case edges match against.
func EvalSwitch(expr string, input ScriptInput, timeout time.Duration) (string, error) {
	vm, stop, err := newVM(input, timeout)
	if err != nil {
		return "", err
	}
	defer stop()
	val, err := vm.RunString("String((function(input){ return (" + expr + "\n); })(input))")
	if err != nil {
		return "", scriptErr(err)
	}
	return val.String(), nil
}

// newVM builds the sandbox: fresh runtime, `input` as plain data, a bounded
// no-op console, the interrupt timer armed.
func newVM(input ScriptInput, timeout time.Duration) (*goja.Runtime, func(), error) {
	vm := goja.New()
	vm.SetMaxCallStackSize(1024)
	raw, err := json.Marshal(input)
	if err != nil {
		return nil, nil, fmt.Errorf("encode script input: %w", err)
	}
	var data any
	if err := json.Unmarshal(raw, &data); err != nil {
		return nil, nil, fmt.Errorf("decode script input: %w", err)
	}
	if err := vm.Set("input", data); err != nil {
		return nil, nil, fmt.Errorf("install script input: %w", err)
	}
	noop := func(goja.FunctionCall) goja.Value { return goja.Undefined() }
	console := vm.NewObject()
	for _, m := range []string{"log", "warn", "error", "debug", "info"} {
		if err := console.Set(m, noop); err != nil {
			return nil, nil, fmt.Errorf("install console: %w", err)
		}
	}
	if err := vm.Set("console", console); err != nil {
		return nil, nil, fmt.Errorf("install console: %w", err)
	}
	timer := time.AfterFunc(timeout, func() { vm.Interrupt("timeout") })
	return vm, func() { timer.Stop() }, nil
}

// scriptErr flattens goja's error types into operator-readable messages.
func scriptErr(err error) error {
	if _, ok := err.(*goja.InterruptedError); ok {
		return fmt.Errorf("script timeout")
	}
	if ex, ok := err.(*goja.Exception); ok {
		return fmt.Errorf("script threw: %s", ex.Value().String())
	}
	return err
}
