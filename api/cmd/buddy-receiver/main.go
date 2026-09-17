// Command buddy-receiver is the standalone Buddy Backup receiver: a container
// with two plain volumes (the backup store and the peer registry) and no ZFS, no
// Kubernetes and no database. It speaks exactly the protocol a Naslos instance
// speaks, so a sender does not know - or care - which kind of receiver it has.
//
// It stores only ciphertext: the keys never leave the sender.
package main

import (
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/buddy"
)

// version is stamped at build time (see api/Dockerfile.receiver).
var version = "dev"

func main() {
	health := flag.Bool("health", false, "probe the running receiver and exit (for container healthchecks)")
	flag.Parse()

	listen := env("BUDDY_LISTEN", ":8484")
	if *health {
		if err := probe(env("BUDDY_HEALTH_URL", "http://127.0.0.1"+normalizeListen(listen)+buddy.PathPrefix+"/health")); err != nil {
			fmt.Fprintf(os.Stderr, "unhealthy: %v\n", err)
			os.Exit(1)
		}
		return
	}

	storePath := env("BUDDY_RECEIVE_PATH", "/data")
	peersPath := env("BUDDY_PEERS", "/config/peers.json")

	peers := buddy.NewPeerStore(peersPath)
	if err := peers.Load(); err != nil {
		log.Fatalf("cannot read the peer registry at %s: %v", peersPath, err)
	}

	auth := buddy.NewAuthenticator(peers)
	// Keep the replay cache across restarts: a request captured just before a
	// restart could otherwise be replayed inside the clock-skew window (NAS-014).
	auth.PersistNonces(storePath)

	receiver := &buddy.Receiver{
		Store:       buddy.NewStore(storePath),
		Peers:       peers,
		Auth:        auth,
		Name:        env("BUDDY_NAME", "buddy-receiver"),
		Version:     version,
		EnrollToken: env("BUDDY_ENROLL_TOKEN", ""),
	}

	mux := http.NewServeMux()
	mux.HandleFunc(buddy.PathPrefix+"/health", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"status":"ok"}`))
	})
	mux.Handle(buddy.PathPrefix+"/", receiver.Handler())

	free, err := receiver.Store.FreeSpace()
	if err != nil {
		log.Fatalf("cannot use the store at %s: %v", storePath, err)
	}
	log.Printf("buddy receiver %s (%s) listening on %s", receiver.Name, version, listen)
	log.Printf("  store:      %s (%s free)", storePath, humanBytes(free))
	log.Printf("  peers:      %s (%d authorized)", peersPath, peers.Count())
	if receiver.EnrollToken != "" {
		log.Printf("  enrollment: open - the first key that presents the token is authorized")
	} else {
		log.Printf("  enrollment: closed - authorize keys in %s", peersPath)
	}

	server := &http.Server{
		Addr:              listen,
		Handler:           mux,
		ReadHeaderTimeout: 15 * time.Second,
		// No write timeout: a chunk upload is bounded, but a slow link should be
		// allowed to finish rather than being cut off mid-backup.
		ReadTimeout: 15 * time.Minute,
		IdleTimeout: 2 * time.Minute,
	}
	if err := server.ListenAndServe(); err != nil {
		log.Fatalf("receiver stopped: %v", err)
	}
}

// env reads an environment variable with a default.
func env(key, fallback string) string {
	if value, ok := os.LookupEnv(key); ok && value != "" {
		return value
	}
	return fallback
}

// normalizeListen turns ":8484" into ":8484" for the health probe's URL.
func normalizeListen(listen string) string {
	if listen == "" {
		return ":8484"
	}
	return listen
}

// probe checks that the receiver answers.
func probe(url string) error {
	client := &http.Client{Timeout: 5 * time.Second}
	resp, err := client.Get(url)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("%s returned %d", url, resp.StatusCode)
	}
	return nil
}

// humanBytes renders a byte count for the start-up log.
func humanBytes(n int64) string {
	const unit = 1024
	if n < unit {
		return fmt.Sprintf("%d B", n)
	}
	div, exp := int64(unit), 0
	for m := n / unit; m >= unit; m /= unit {
		div *= unit
		exp++
	}
	return fmt.Sprintf("%.1f %ciB", float64(n)/float64(div), "KMGTPE"[exp])
}
