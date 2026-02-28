import { clusterName, clusterEndpoint, clusterLocation, clusterCaCert } from "./cluster";
import { registryUrl } from "./registry";
import { controllerServiceAccountEmail } from "./identity";

// Stack outputs consumed by the services stack via StackReference
export const cluster = clusterName;
export const clusterRegion = clusterLocation;
export const endpoint = clusterEndpoint;
export const caCert = clusterCaCert;
export const registry = registryUrl;
export const controllerServiceAccount = controllerServiceAccountEmail;
