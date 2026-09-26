package authelia

import (
	"context"
	"strings"
	"testing"

	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
	"k8s.io/client-go/kubernetes/fake"
)

func TestFragmentsCookieAndRuleShape(t *testing.T) {
	cookies, rules, err := Fragments([]string{"naslos.local", "media.example.com"})
	if err != nil {
		t.Fatalf("fragments: %v", err)
	}
	gotCookies := string(cookies)
	for _, want := range []string{
		"domain: naslos.local",
		"authelia_url: https://naslos.local/authelia/",
		"domain: media.example.com",
		"authelia_url: https://media.example.com/authelia/",
	} {
		if !strings.Contains(gotCookies, want) {
			t.Errorf("cookies.yml missing %q:\n%s", want, gotCookies)
		}
	}

	gotRules := string(rules)
	// The primary apex already has static rules, so it is not repeated; the
	// non-primary apex and every wildcard must be present.
	if strings.Contains(gotRules, "domain: naslos.local\n") {
		t.Errorf("rules.yml must not repeat the primary apex:\n%s", gotRules)
	}
	for _, want := range []string{
		"domain: media.example.com\n  policy: one_factor",
		"domain: '*.naslos.local'\n  policy: one_factor",
		"domain: '*.media.example.com'\n  policy: one_factor",
	} {
		if !strings.Contains(gotRules, want) {
			t.Errorf("rules.yml missing %q:\n%s", want, gotRules)
		}
	}
}

func TestFragmentsPrimaryOnly(t *testing.T) {
	cookies, rules, err := Fragments([]string{"naslos.local"})
	if err != nil {
		t.Fatalf("fragments: %v", err)
	}
	if !strings.Contains(string(cookies), "domain: naslos.local") {
		t.Errorf("cookies missing primary:\n%s", cookies)
	}
	if strings.Contains(string(rules), "domain: naslos.local\n") {
		t.Errorf("rules must not contain a primary apex rule:\n%s", rules)
	}
	if !strings.Contains(string(rules), "domain: '*.naslos.local'") {
		t.Errorf("rules missing the primary wildcard:\n%s", rules)
	}
}

func TestFragmentsAreValidYAMLForZeroDomains(t *testing.T) {
	cookies, rules, err := Fragments(nil)
	if err != nil {
		t.Fatalf("fragments: %v", err)
	}
	if strings.TrimSpace(string(cookies)) != "[]" || strings.TrimSpace(string(rules)) != "[]" {
		t.Fatalf("empty list should render []: %q / %q", cookies, rules)
	}
}

// syncHarness builds a reconciler over a fake clientset holding the Authelia
// pod, so the restart delete has a target.
func syncHarness(t *testing.T) (*Reconciler, *fake.Clientset) {
	t.Helper()
	client := fake.NewSimpleClientset(&corev1.Pod{
		ObjectMeta: metav1.ObjectMeta{Name: "naslos-authelia-0", Namespace: "naslos"},
	})
	r := NewReconciler(Options{
		Client:    func() (kubernetes.Interface, error) { return client, nil },
		Namespace: "naslos",
	})
	return r, client
}

// seedFragments writes the fragment ConfigMap with the given content.
func seedFragments(t *testing.T, client *fake.Clientset, cookies, rules string) {
	t.Helper()
	_, err := client.CoreV1().ConfigMaps("naslos").Create(context.Background(), &corev1.ConfigMap{
		ObjectMeta: metav1.ObjectMeta{Name: "naslos-authelia-sso", Namespace: "naslos"},
		Data:       map[string]string{CookiesKey: cookies, RulesKey: rules},
	}, metav1.CreateOptions{})
	if err != nil {
		t.Fatalf("seeding fragments: %v", err)
	}
}

func podExists(client *fake.Clientset) bool {
	_, err := client.CoreV1().Pods("naslos").Get(context.Background(), "naslos-authelia-0", metav1.GetOptions{})
	return err == nil
}

// TestSyncMissingConfigMapIsAnError pins the contract: the chart seeds the
// ConfigMap, and the API must not create it (that would need an unscoped
// `create` on configmaps).
func TestSyncMissingConfigMapIsAnError(t *testing.T) {
	r, client := syncHarness(t)

	if err := r.Sync(context.Background(), []string{"naslos.local"}); err == nil {
		t.Fatal("expected an error when the fragment ConfigMap is missing")
	}
	if !podExists(client) {
		t.Fatal("a missing ConfigMap must not restart Authelia")
	}
}

func TestSyncIsIdempotentWhenUnchanged(t *testing.T) {
	r, client := syncHarness(t)
	cookies, rules, _ := Fragments([]string{"naslos.local"})
	seedFragments(t, client, string(cookies), string(rules))

	if err := r.Sync(context.Background(), []string{"naslos.local"}); err != nil {
		t.Fatalf("sync: %v", err)
	}
	if !podExists(client) {
		t.Fatal("an unchanged sync restarted Authelia")
	}
}

func TestSyncRewritesAndRestartsOnChange(t *testing.T) {
	r, client := syncHarness(t)
	cookies, rules, _ := Fragments([]string{"naslos.local"})
	seedFragments(t, client, string(cookies), string(rules))

	if err := r.Sync(context.Background(), []string{"naslos.local", "media.example.com"}); err != nil {
		t.Fatalf("promote sync: %v", err)
	}
	cm, err := client.CoreV1().ConfigMaps("naslos").Get(context.Background(), "naslos-authelia-sso", metav1.GetOptions{})
	if err != nil {
		t.Fatalf("get cm: %v", err)
	}
	if !strings.Contains(cm.Data[CookiesKey], "media.example.com") ||
		!strings.Contains(cm.Data[RulesKey], "*.media.example.com") {
		t.Fatalf("fragments were not updated: %+v", cm.Data)
	}
	if podExists(client) {
		t.Fatal("promotion did not delete the Authelia pod")
	}
}

// TestSyncAdoptsSemanticallyEqualSeed proves a Helm-rendered seed (different
// quoting/formatting) is treated as unchanged, so a startup reconcile does not
// needlessly interrupt auth.
func TestSyncAdoptsSemanticallyEqualSeed(t *testing.T) {
	r, client := syncHarness(t)
	seedFragments(t, client,
		"- domain: \"naslos.local\"\n  authelia_url: \"https://naslos.local/authelia/\"\n",
		"- domain: \"*.naslos.local\"\n  policy: \"one_factor\"\n",
	)

	if err := r.Sync(context.Background(), []string{"naslos.local"}); err != nil {
		t.Fatalf("sync: %v", err)
	}
	if !podExists(client) {
		t.Fatal("an unchanged seed must not restart Authelia")
	}
}
