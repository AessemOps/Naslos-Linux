package helm

import (
	"context"
	"fmt"
	"strings"
	"time"

	"helm.sh/helm/v3/pkg/action"
	"helm.sh/helm/v3/pkg/chart"
	"helm.sh/helm/v3/pkg/chart/loader"
	"helm.sh/helm/v3/pkg/release"
)

// InstallDir installs a chart from a local directory. Chart repositories are
// cloned by the chartsrepo package, so there is no Helm repo index involved.
func (c *Client) InstallDir(ctx context.Context, name, chartDir string, values map[string]interface{}) (*release.Release, error) {
	chart, err := loader.LoadDir(chartDir)
	if err != nil {
		return nil, fmt.Errorf("loading chart from %s: %w", chartDir, err)
	}
	return c.install(ctx, name, chart, values)
}

// UpgradeDir upgrades a release from a local chart directory. The supplied
// values replace the previous set (ResetValues) so a reconfigure round-trips
// exactly what the UI shows.
func (c *Client) UpgradeDir(ctx context.Context, name, chartDir string, values map[string]interface{}) (*release.Release, error) {
	chart, err := loader.LoadDir(chartDir)
	if err != nil {
		return nil, fmt.Errorf("loading chart from %s: %w", chartDir, err)
	}
	return c.upgrade(ctx, name, chart, values)
}

func (c *Client) install(ctx context.Context, name string, chart *chart.Chart, values map[string]interface{}) (*release.Release, error) {
	config, err := c.getActionConfig(c.namespace)
	if err != nil {
		return nil, err
	}

	install := action.NewInstall(config)
	install.Namespace = c.namespace
	install.ReleaseName = name
	install.CreateNamespace = false
	install.Wait = true
	install.Timeout = 5 * time.Minute

	rel, err := install.RunWithContext(ctx, chart, values)
	if err != nil {
		return nil, fmt.Errorf("installing chart: %w", err)
	}
	return rel, nil
}

func (c *Client) upgrade(ctx context.Context, name string, chart *chart.Chart, values map[string]interface{}) (*release.Release, error) {
	config, err := c.getActionConfig(c.namespace)
	if err != nil {
		return nil, err
	}

	upgrade := action.NewUpgrade(config)
	upgrade.Namespace = c.namespace
	upgrade.Wait = true
	upgrade.Timeout = 5 * time.Minute
	upgrade.ResetValues = true

	rel, err := upgrade.RunWithContext(ctx, name, chart, values)
	if err != nil {
		return nil, fmt.Errorf("upgrading chart: %w", err)
	}
	return rel, nil
}

// Uninstall uninstalls a Helm release.
func (c *Client) Uninstall(ctx context.Context, name string) error {
	config, err := c.getActionConfig(c.namespace)
	if err != nil {
		return err
	}

	uninstall := action.NewUninstall(config)
	uninstall.Wait = true
	uninstall.Timeout = 5 * time.Minute

	_, err = uninstall.Run(name)
	if err != nil {
		return fmt.Errorf("uninstalling release: %w", err)
	}
	return nil
}

// List lists all Helm releases in the namespace.
func (c *Client) List(ctx context.Context) ([]App, error) {
	config, err := c.getActionConfig(c.namespace)
	if err != nil {
		return nil, err
	}

	list := action.NewList(config)
	list.SetStateMask()

	releases, err := list.Run()
	if err != nil {
		return nil, fmt.Errorf("listing releases: %w", err)
	}

	apps := make([]App, 0, len(releases))
	for _, rel := range releases {
		app := App{
			Name:      rel.Name,
			Namespace: rel.Namespace,
			Chart:     rel.Chart.Name(),
			Version:   rel.Chart.Metadata.Version,
			Status:    ReleaseStatus(rel.Info.Status.String()),
			UpdatedAt: rel.Info.LastDeployed.Time,
		}
		if rel.Chart.Metadata.Description != "" {
			app.Description = rel.Chart.Metadata.Description
		}
		apps = append(apps, app)
	}

	return apps, nil
}

// Get returns details of a specific release.
func (c *Client) Get(ctx context.Context, name string) (*App, error) {
	config, err := c.getActionConfig(c.namespace)
	if err != nil {
		return nil, err
	}

	get := action.NewGet(config)
	rel, err := get.Run(name)
	if err != nil {
		return nil, fmt.Errorf("getting release: %w", err)
	}

	app := &App{
		Name:      rel.Name,
		Namespace: rel.Namespace,
		Chart:     rel.Chart.Name(),
		Version:   rel.Chart.Metadata.Version,
		Status:    ReleaseStatus(rel.Info.Status.String()),
		UpdatedAt: rel.Info.LastDeployed.Time,
		Values:    rel.Config,
	}
	if rel.Chart.Metadata.Description != "" {
		app.Description = rel.Chart.Metadata.Description
	}

	return app, nil
}

// Rollback rolls back a release to a previous revision.
func (c *Client) Rollback(ctx context.Context, name string, revision int) error {
	config, err := c.getActionConfig(c.namespace)
	if err != nil {
		return err
	}

	rollback := action.NewRollback(config)
	rollback.Version = revision
	rollback.Wait = true
	rollback.Timeout = 5 * time.Minute

	return rollback.Run(name)
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
