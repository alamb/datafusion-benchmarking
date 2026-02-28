import * as gcp from "@pulumi/gcp";
import * as pulumi from "@pulumi/pulumi";

const gcpConfig = new pulumi.Config("gcp");
const region = gcpConfig.require("region");

export const registry = new gcp.artifactregistry.Repository("benchmarking", {
  repositoryId: "benchmarking",
  location: region,
  format: "DOCKER",
  description: "Container images for DataFusion benchmarking",
});

export const registryUrl = pulumi.interpolate`${region}-docker.pkg.dev/${gcp.config.project}/${registry.repositoryId}`;
