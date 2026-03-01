import * as gcp from "@pulumi/gcp";
import * as pulumi from "@pulumi/pulumi";

const project = gcp.config.project!;
const gcpConfig = new pulumi.Config("gcp");
const region = gcpConfig.require("region");

export const registry = new gcp.artifactregistry.Repository(
  "benchmarking",
  {
    repositoryId: "benchmarking",
    location: region,
    format: "DOCKER",
    description: "Container images for DataFusion benchmarking",
  },
);

export const registryUrl = pulumi.interpolate`${region}-docker.pkg.dev/${project}/${registry.repositoryId}`;

// GCS bucket for sccache compiled artifact caching
export const sccacheBucket = new gcp.storage.Bucket("sccache", {
  name: `${project}-sccache`,
  location: region,
  uniformBucketLevelAccess: true,
  lifecycleRules: [{
    action: { type: "Delete" },
    condition: { age: 7 },
  }],
});
