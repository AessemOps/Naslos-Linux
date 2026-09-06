package main

import (
	"context"
	"flag"
	"log"
	"net/http"
	"os"
	"os/signal"
	"syscall"

	"github.com/nasos/nasos/agent/internal/zfs"
	"github.com/nasos/nasos/agent/internal/server"
)

// nasos-agent runs as a privileged DaemonSet on each node.
// It executes zpool/zfs commands via chroot /host to manage ZFS pools,
// since ZFS pools live outside Talos's volume system.

func main() {
	var (
		node   string
		listen string
	)
	flag.StringVar(&node, "node", os.Getenv("NODE_NAME"), "Node name this agent runs on")
	flag.StringVar(&listen, "listen", ":9090", "HTTP listen address for agent API")
	flag.Parse()

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	log.Printf("nasos-agent starting on node %s", node)

	// Verify ZFS is available on the host
	if !zfs.IsZFSAvailable() {
		log.Fatalf("ZFS not available on host — is the zfs extension installed?")
	}

	// Create ZFS client
	zfsClient := zfs.NewClient(ctx)

	// Import any existing pools (idempotent)
	if err := zfsClient.ImportPool(""); err != nil {
		log.Printf("Note: zpool import returned: %v", err)
	}

	// Start HTTP server for pool operations
	srv := server.New(listen, zfsClient)
	go func() {
		if err := srv.Start(); err != nil && err != http.ErrServerClosed {
			log.Fatalf("Agent server error: %v", err)
		}
	}()

	log.Printf("nasos-agent listening on %s", listen)

	<-ctx.Done()
	log.Println("Shutting down...")
	srv.Shutdown(context.Background())
}
