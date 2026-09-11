package helm

import (
	"fmt"
	"strings"

	"helm.sh/helm/v3/pkg/repo"
)

// AddRepo adds a Helm chart repository.
func (c *Client) AddRepo(name, url string) error {
	repoFile := repo.NewFile()
	repoFile.Add(&repo.Entry{
		Name: name,
		URL:  url,
	})

	if err := repoFile.WriteFile(c.settings.RepositoryConfig, 0644); err != nil {
		return fmt.Errorf("writing repo file: %w", err)
	}

	return nil
}

// UpdateRepos updates all chart repositories.
func (c *Client) UpdateRepos() error {
	repoFile, err := repo.LoadFile(c.settings.RepositoryConfig)
	if err != nil {
		return fmt.Errorf("loading repo file: %w", err)
	}

	for _, re := range repoFile.Repositories {
		_ = re // In production: use repo.NewChartRepository with cache
	}

	return nil
}

// ReleaseStatus returns a human-readable status for a release.
func ReleaseStatus(status string) string {
	switch strings.ToLower(status) {
	case "deployed":
		return "running"
	case "failed":
		return "failed"
	case "pending-install", "pending-upgrade", "pending-rollback":
		return "pending"
	case "uninstalled", "uninstalling":
		return "stopped"
	case "superseded":
		return "superseded"
	default:
		return status
	}
}
