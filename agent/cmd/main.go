package main

import (
	"context"
	"flag"
	"log"
	"net/http"
	"os"
	"os/signal"
	"syscall"

	"github.com/AessemOps/Naslos-Linux/agent/internal/server"
	"github.com/AessemOps/Naslos-Linux/agent/internal/shares"
	"github.com/AessemOps/Naslos-Linux/agent/internal/zfs"
)

// naslos-agent runs as a privileged DaemonSet on each node.
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

	log.Printf("naslos-agent starting on node %s", node)

	// ZFS is optional: on nodes without the ZFS system extension (e.g. the
	// stock Talos installer used for the single-node VM), the agent starts in
	// degraded mode — its ZFS endpoints return 503 instead of crash-looping.
	var zfsClient *zfs.Client
	if !zfs.IsZFSAvailable() {
		log.Printf("WARN: ZFS not available on host — starting in degraded mode (ZFS endpoints return 503)")
	} else {
		zfsClient = zfs.NewClient(ctx)

		// Import any existing pools (idempotent)
		if err := zfsClient.ImportPool(""); err != nil {
			log.Printf("Note: zpool import returned: %v", err)
		}
	}

	// Share configuration is optional too: it needs the host root mounted at
	// /host. Without it the agent still serves ZFS operations and the shares
	// endpoints return 503.
	var sharesClient *shares.Client
	if !shares.IsAvailable() {
		log.Printf("WARN: host root not available at /host — share configuration endpoints return 503")
	} else {
		sharesClient = shares.NewClient(ctx)
	}

	// Start HTTP server for pool and share configuration operations
	srv := server.New(listen, zfsClient, sharesClient)
	go func() {
		if err := srv.Start(); err != nil && err != http.ErrServerClosed {
			log.Fatalf("Agent server error: %v", err)
		}
	}()

	log.Printf("naslos-agent listening on %s", listen)

	<-ctx.Done()
	log.Println("Shutting down...")
	srv.Shutdown(context.Background())
}
