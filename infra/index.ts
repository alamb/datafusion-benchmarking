import * as pulumi from "@pulumi/pulumi";

import { clusterName, clusterEndpoint, clusterLocation } from "./cluster";
import { registryUrl } from "./registry";
import {
  wifProviderName,
  ghaServiceAccountEmail,
  controllerServiceAccountEmail,
} from "./identity";
import { controllerStatefulSet } from "./controller";

// Stack outputs used by GitHub Actions workflows
export const cluster = clusterName;
export const clusterRegion = clusterLocation;
export const endpoint = clusterEndpoint;
export const registry = registryUrl;
export const wifProvider = wifProviderName;
export const ghaServiceAccount = ghaServiceAccountEmail;
export const controllerServiceAccount = controllerServiceAccountEmail;
