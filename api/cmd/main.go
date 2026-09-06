package main

import (
	"context"
	"flag"
	"log"
	"os"
	"os/signal"
	"syscall"

	"github.com/nasos/nasos/api/internal/config"
	"github.com/nasos/nasos/api/internal/server"
	"github.com/nasos/nasos/api/internal/talos"
)

func main() {
	var cfg config.Config
	flag.StringVar(&cfg.ListenAddr, "listen", ":8080", "HTTP listen address")
	flag.StringVar(&cfg.Kubeconfig, "kubeconfig", "", "Path to kubeconfig (empty = in-cluster)")
	flag.StringVar(&cfg.TalosConfig, "talosconfig", "", "Path to talosconfig (empty = use default)")
	flag.Parse()

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	tc, err := talos.NewClient(ctx, cfg.TalosConfig)
	if err != nil {
		log.Fatalf("Failed to create Talos client: %v", err)
	}
	defer tc.Close()

	srv := server.New(cfg.ListenAddr, tc)

	go func() {
		if err := srv.Start(); err != nil {
			log.Printf("Server stopped: %v", err)
			stop()
		}
	}()

	<-ctx.Done()
	log.Println("Shutting down...")
	srv.Shutdown(context.Background())
	os.Exit(0)
}
