import * as gcp from "@pulumi/gcp";
import * as pulumi from "@pulumi/pulumi";
import { registry } from "./registry";

const project = gcp.config.project!;
const gcpConfig = new pulumi.Config("gcp");
const region = gcpConfig.require("region");

// -----------------------------------------------------------------------
// Bootstrap resources (WIF pool, WIF provider, gha-deployer SA) are
// managed manually via bootstrap_oidc.sh because they are prerequisites
// for GitHub Actions to authenticate to GCP — Pulumi can't create the
// resources it needs to run.
//
// However, IAM *bindings* that reference Pulumi-managed resources (like
// the Artifact Registry repo) belong here so we can scope them narrowly.
// -----------------------------------------------------------------------

// Reference the bootstrap-created gha-deployer service account
const ghaDeployerEmail = `gha-deployer@${project}.iam.gserviceaccount.com`;

// --- GHA deployer: scoped Artifact Registry access ---
// Writer on specifically our benchmarking repo, not the whole project.

new gcp.artifactregistry.RepositoryIamMember("gha-registry-writer", {
  project,
  location: region,
  repository: registry.repositoryId,
  role: "roles/artifactregistry.writer",
  member: `serviceAccount:${ghaDeployerEmail}`,
});

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

// --- Workload Identity binding for controller K8s SA ---
// Allows the K8s service account in the benchmarking namespace to
// impersonate the GCP controller service account.

export const controllerWiBinding = new gcp.serviceaccount.IAMMember("controller-wi-binding", {
  serviceAccountId: controllerSa.name,
  role: "roles/iam.workloadIdentityUser",
  member: pulumi.interpolate`serviceAccount:${project}.svc.id.goog[benchmarking/benchmark-controller]`,
});

// --- Outputs ---

export const controllerServiceAccountEmail = controllerSa.email;
