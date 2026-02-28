import * as pulumi from "@pulumi/pulumi";

import { clusterName, clusterEndpoint, clusterLocation } from "./cluster";
import { registryUrl } from "./registry";
import { controllerServiceAccountEmail } from "./identity";
import { controllerStatefulSet } from "./controller";

// Stack outputs used by GitHub Actions workflows
export const cluster = clusterName;
export const clusterRegion = clusterLocation;
export const endpoint = clusterEndpoint;
export const registry = registryUrl;
export const controllerServiceAccount = controllerServiceAccountEmail;
