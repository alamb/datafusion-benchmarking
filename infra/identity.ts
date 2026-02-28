import * as gcp from "@pulumi/gcp";
import * as pulumi from "@pulumi/pulumi";
import { cluster } from "./cluster";

const config = new pulumi.Config();
const project = gcp.config.project!;

// --- Controller service account (used by the controller pod via Workload Identity) ---

export const controllerSa = new gcp.serviceaccount.Account("benchmark-controller", {
  accountId: "benchmark-controller",
  displayName: "Benchmark Controller",
});

// Controller needs to create/watch/delete K8s Jobs
new gcp.projects.IAMMember("controller-container-developer", {
  project,
  role: "roles/container.developer",
  member: pulumi.interpolate`serviceAccount:${controllerSa.email}`,
});

// --- GHA deployer service account ---

export const ghaSa = new gcp.serviceaccount.Account("gha-deployer", {
  accountId: "gha-deployer",
  displayName: "GitHub Actions Deployer",
});

const ghaRoles = [
  "roles/container.admin",
  "roles/artifactregistry.writer",
  "roles/iam.serviceAccountUser",
];

for (const role of ghaRoles) {
  const safeName = role.replace(/\//g, "-").replace(/\./g, "-");
  new gcp.projects.IAMMember(`gha-${safeName}`, {
    project,
    role,
    member: pulumi.interpolate`serviceAccount:${ghaSa.email}`,
  });
}

// --- Workload Identity Federation for GitHub Actions ---

export const wifPool = new gcp.iam.WorkloadIdentityPool("github-actions", {
  workloadIdentityPoolId: "github-actions",
  displayName: "GitHub Actions",
});

export const wifProvider = new gcp.iam.WorkloadIdentityPoolProvider("github-oidc", {
  workloadIdentityPoolId: wifPool.workloadIdentityPoolId,
  workloadIdentityPoolProviderId: "github-oidc",
  displayName: "GitHub OIDC",
  attributeMapping: {
    "google.subject": "assertion.sub",
    "attribute.repository": "assertion.repository",
    "attribute.actor": "assertion.actor",
  },
  attributeCondition: 'assertion.repository == "adriang/datafusion-benchmarking"',
  oidc: {
    issuerUri: "https://token.actions.githubusercontent.com",
  },
});

// Allow the WIF pool to impersonate the GHA service account
new gcp.serviceaccount.IAMMember("gha-wif-binding", {
  serviceAccountId: controllerSa.name,
  role: "roles/iam.workloadIdentityUser",
  member: pulumi.interpolate`principalSet://iam.googleapis.com/${wifPool.name}/attribute.repository/adriang/datafusion-benchmarking`,
});

new gcp.serviceaccount.IAMMember("gha-deployer-wif-binding", {
  serviceAccountId: ghaSa.name,
  role: "roles/iam.workloadIdentityUser",
  member: pulumi.interpolate`principalSet://iam.googleapis.com/${wifPool.name}/attribute.repository/adriang/datafusion-benchmarking`,
});

// --- Workload Identity binding for controller K8s SA ---

export const controllerWiBinding = new gcp.serviceaccount.IAMMember("controller-wi-binding", {
  serviceAccountId: controllerSa.name,
  role: "roles/iam.workloadIdentityUser",
  member: pulumi.interpolate`serviceAccount:${project}.svc.id.goog[benchmarking/benchmark-controller]`,
});

// --- Outputs ---

export const wifProviderName = wifProvider.name;
export const ghaServiceAccountEmail = ghaSa.email;
export const controllerServiceAccountEmail = controllerSa.email;
