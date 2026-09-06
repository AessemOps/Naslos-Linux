package main

import (
	"context"
	"flag"
	"log"
	"os"
	"os/exec"
	"os/signal"
	"strings"
	"syscall"
)

// nasos-agent runs as a privileged DaemonSet on each node.
// It executes zpool/zfs commands via chroot /host to manage ZFS pools,
// since ZFS pools live outside Talos's volume system.

const hostRoot = "/host"

func main() {
	var node string
	flag.StringVar(&node, "node", os.Getenv("NODE_NAME"), "Node name this agent runs on")
	flag.Parse()

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	log.Printf("nasos-agent starting on node %s", node)

	// Verify ZFS module is loaded
	if err := hostExec("zpool", "version"); err != nil {
		log.Fatalf("ZFS not available on host: %v", err)
	}

	// Import any existing pools (idempotent)
	if err := hostExec("zpool", "import", "-fal"); err != nil {
		log.Printf("Note: zpool import returned: %v", err)
	}

	// Start API server for pool operations
	<-ctx.Done()
	log.Println("Shutting down...")
}

// hostExec runs a command inside the host namespace via chroot.
func hostExec(name string, args ...string) error {
	cmd := exec.CommandContext(context.Background(), "chroot", hostRoot, name)
	cmd.Args = append(cmd.Args, args...)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	log.Printf("host exec: %s %s", name, strings.Join(args, " "))
	return cmd.Run()
}
