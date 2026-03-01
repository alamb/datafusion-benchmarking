import * as gcp from "@pulumi/gcp";
import * as pulumi from "@pulumi/pulumi";
import { registry, sccacheBucket } from "./registry";

const project = gcp.config.project!;
const gcpConfig = new pulumi.Config("gcp");
const region = gcpConfig.require("region");

// -----------------------------------------------------------------------
// Controller service account and IAM. WIF pool, OIDC provider, and
// gha-deployer SA are managed by bootstrap.sh (gcloud), not Pulumi.
// -----------------------------------------------------------------------

// --- GHA deployer: scoped Artifact Registry access ---
// The gha-deployer SA is created by bootstrap.sh. We reference it by
// email to grant scoped AR writer access on our specific repository.

const ghaDeployerEmail = `gha-deployer@${project}.iam.gserviceaccount.com`;

new gcp.artifactregistry.RepositoryIamMember("gha-registry-writer", {
  project,
  location: region,
  repository: registry.repositoryId,
  role: "roles/artifactregistry.writer",
  member: `serviceAccount:${ghaDeployerEmail}`,
});

// --- Controller service account (used by the controller pod via Workload Identity) ---

export const controllerSa = new gcp.serviceaccount.Account(
  "benchmark-controller",
  {
    accountId: "benchmark-controller",
    displayName: "Benchmark Controller",
  },
);

// Controller needs to create/watch/delete K8s Jobs
new gcp.projects.IAMMember("controller-container-developer", {
  project,
  role: "roles/container.developer",
  member: pulumi.interpolate`serviceAccount:${controllerSa.email}`,
});

// --- Workload Identity binding for controller K8s SA ---
// Allows the K8s service account in the benchmarking namespace to
// impersonate the GCP controller service account.

export const controllerWiBinding = new gcp.serviceaccount.IAMMember(
  "controller-wi-binding",
  {
    serviceAccountId: controllerSa.name,
    role: "roles/iam.workloadIdentityUser",
    member: pulumi.interpolate`serviceAccount:${project}.svc.id.goog[benchmarking/benchmark-controller]`,
  }
);

// --- Benchmark runner service account (used by runner pods via Workload Identity) ---

export const runnerSa = new gcp.serviceaccount.Account(
  "benchmark-runner",
  {
    accountId: "benchmark-runner",
    displayName: "Benchmark Runner",
  },
);

// Runner needs read/write access to the sccache GCS bucket
new gcp.storage.BucketIAMMember("runner-sccache-admin", {
  bucket: sccacheBucket.name,
  role: "roles/storage.objectAdmin",
  member: pulumi.interpolate`serviceAccount:${runnerSa.email}`,
});

// Workload Identity binding for runner K8s SA
export const runnerWiBinding = new gcp.serviceaccount.IAMMember(
  "runner-wi-binding",
  {
    serviceAccountId: runnerSa.name,
    role: "roles/iam.workloadIdentityUser",
    member: pulumi.interpolate`serviceAccount:${project}.svc.id.goog[benchmarking/benchmark-runner]`,
  },
);

// --- Outputs ---

export const controllerServiceAccountEmail = controllerSa.email;
export const runnerServiceAccountEmail = runnerSa.email;
