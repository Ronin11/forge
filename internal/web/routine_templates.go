package web

// Built-in routine templates: curated starting points for common working roles.
// A template is not a routine — it pre-fills the New-routine form (mode, prompt,
// model, class) with role-appropriate defaults so an operator only fills in the
// specific objective and the repositories, then saves. The prompt is a scaffold:
// it frames the role and ends with a bracketed placeholder to replace. Modes
// supply the workflow and result schema (see internal/modes); the template's
// prompt is the role/task framing layered on top (§ "Prompt assembly").

import "net/http"

// RoutineTemplate is one role starting point.
type RoutineTemplate struct {
	Key         string `json:"key"`          // slug used to seed the routine name
	Role        string `json:"role"`         // display label
	Description string `json:"description"`  // one line: what this role does
	Mode        string `json:"mode"`         // execution mode (internal/modes)
	Model       string `json:"model"`        // default model alias
	BudgetClass string `json:"budget_class"` // default class
	Integrate   bool   `json:"integrate"`    // merge on success (implementers only)
	Prompt      string `json:"prompt"`       // role framing + objective placeholder
}

// routineTemplates is the shipped set. Prompts stay tight — the mode already
// carries the how; these carry the who and leave a placeholder for the what.
var routineTemplates = []RoutineTemplate{
	{
		Key: "product-manager", Role: "Product Manager", Mode: "plan", Model: "sonnet", BudgetClass: "normal",
		Description: "Turn a goal into a scoped, prioritized set of buildable tasks.",
		Prompt:      "Act as a product manager for {{repo}}. Take the objective below and turn it into a crisp, buildable plan: state the intent in one line, define what is in scope and explicitly what is out, write acceptance criteria a verifier can check, and break the work into ordered, independently-shippable tasks with a priority for each. Surface open questions for a human rather than guessing.\n\nObjective: [replace with the goal to break down]",
	},
	{
		Key: "architect", Role: "Architect", Mode: "explore", Model: "sonnet", BudgetClass: "normal",
		Description: "Study the codebase and propose a design + implementation plan. Read-only.",
		Prompt:      "Act as a software architect for {{repo}}. For the objective below, read the code it touches and search forge_kb_search for prior design notes, then propose an approach: the design, the components and their boundaries, the key decisions and their trade-offs, the risks, and a step-by-step implementation plan someone else could follow. Do not write code — produce a design note.\n\nObjective: [replace with what needs designing]",
	},
	{
		Key: "programmer", Role: "Programmer", Mode: "implement", Model: "sonnet", BudgetClass: "normal", Integrate: true,
		Description: "Implement a task in small commits with tests, making the checks pass.",
		Prompt:      "Act as a careful programmer on {{repo}}. Implement the task below by following this repository's existing conventions and patterns. Work in small commits, add tests for the behavior you change, and make the declared checks pass before finishing; if a check fails, fix your own change rather than unrelated code.\n\nTask: [replace with the specific task to implement]",
	},
	{
		Key: "reviewer", Role: "Reviewer", Mode: "review", Model: "sonnet", BudgetClass: "normal",
		Description: "Review the current change for correctness, risk, and fit. Report, don't fix.",
		Prompt:      "Act as a code reviewer on {{repo}}. Review the current change for correctness, edge cases, security, performance, and fit with the repository's conventions. Report findings ranked by severity, each with a file:line and a concrete suggested fix. Do not modify code — the output is the review.\n\nFocus (optional): [replace with anything to weight, or leave blank for a full review]",
	},
	{
		Key: "qa-tester", Role: "QA Tester", Mode: "audit", Model: "sonnet", BudgetClass: "normal",
		Description: "Exercise a feature against its acceptance criteria and report bugs. Read-only.",
		Prompt:      "Act as a QA tester for {{repo}}. Exercise the feature or change below against its intended behavior, probe edge cases and failure modes, and report every defect you find with steps to reproduce and expected-vs-actual. Do not fix anything — report the findings.\n\nWhat to test: [replace with the feature/change and its acceptance criteria]",
	},
	{
		Key: "end-user", Role: "End User", Mode: "explore", Model: "sonnet", BudgetClass: "normal",
		Description: "Walk the product as a real user and report friction. Judges UX, not code.",
		Prompt:      "Act as a real end user of {{repo}} — not a developer. Walk through the main user flows for the scenario below, note friction, confusion, dead ends, and anything that feels broken from a user's point of view, and suggest concrete improvements. Judge the experience, not the implementation.\n\nScenario: [replace with the user goal / flow to try]",
	},
	{
		Key: "tech-writer", Role: "Tech Writer", Mode: "docs", Model: "sonnet", BudgetClass: "normal",
		Description: "Write or update documentation to match the repo's style.",
		Prompt:      "Act as a technical writer for {{repo}}. Write or update the documentation for the topic below so a new contributor can follow it: accurate, concise, and matching the repository's existing docs style and structure. Verify claims against the code as you go.\n\nTopic: [replace with what to document]",
	},
	{
		Key: "security-auditor", Role: "Security Auditor", Mode: "audit", Model: "opus", BudgetClass: "normal",
		Description: "Review code for vulnerabilities and report remediations. Read-only.",
		Prompt:      "Act as a security auditor for {{repo}}. Review the code in scope for vulnerabilities — injection, broken authz, exposed secrets, unsafe deserialization, SSRF, path traversal, and the like — and report each finding with its severity, the affected file:line, the impact, and a concrete remediation. Do not exploit or modify anything; the output is the audit.\n\nScope: [replace with the area to audit, or leave blank for the whole repo]",
	},
}

// listRoutineTemplates is GET /api/v1/routine-templates — the built-in role
// starting points the New-routine dialog offers.
func (s *Server) listRoutineTemplates(*http.Request) (int, any, error) {
	return http.StatusOK, routineTemplates, nil
}
