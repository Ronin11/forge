package main

import (
	"syscall"

	"forge/internal/worker"
)

func syscallKill0(pid int) error { return syscall.Kill(pid, 0) }
func runFixture() worker.RepoRun { return worker.RepoRun{PortEnv: "PORT"} }
