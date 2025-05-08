# README

Welcome to [Miles Storm's Personal Website](https://milesstorm.com)

This repository contains the code and configuration for my personal website, which serves as a showcase of various technologies and my expertise in microservices architecture, DevOps, and web development. The website is designed with a robust and scalable architecture, utilizing a diverse set of tools and frameworks to ensure high availability and performance.

## Architecture Overview

The website's architecture leverages a comprehensive microservices setup, incorporating numerous cutting-edge technologies to deliver a resilient and efficient system. Below is a list of the key technologies used:

- **Istio**: Service mesh for managing microservices traffic, providing observability, and enhancing security.
- **Kubernetes**: Orchestrates containerized applications for automated deployment, scaling, and management.
- **Longhorn**: Cloud-native distributed block storage for managing persistent volumes.
- **Cert-manager**: Automates the management and issuance of TLS certificates.
- **ArgoCD**: Declarative GitOps continuous delivery tool for Kubernetes.
- **k3s**: Lightweight Kubernetes (k3s) for multi-cluster with agent based architecture.
- **Metallb**: Load-balancer implementation for Kubernetes clusters.
- **Grafana**: Analytics and monitoring platform for visualizing metrics.
- **Prometheus**: Monitoring system and time series database.
- **Distributed Postgres**: Highly available and scalable relational database setup.

## Web Application

The website itself is built using modern web development technologies:

- **Rust Axum Webserver**: High-performance, asynchronous web server framework.
- **Rust Dioxus Frontend**: Reactive UI library for building dynamic, interactive user interfaces.

## Upcoming Features

I am continuously working on enhancing the functionality of my website. The following features are in the pipeline:

- **PyTorch Dice Recognition Bot**: An AI-powered bot using PyTorch for recognizing dice rolls.
- **And Many More**: Stay tuned for additional features and improvements!

## Getting Started

To run the website locally, follow these steps:

1. **Clone the repository**:
    ```sh
    git clone https://github.com/yourusername/yourrepository.git
    cd yourrepository
    ```

2. **Set up the Kubernetes cluster**:
    Follow the instructions in the `kubernetes/README.md` file to set up the local Kubernetes cluster using k3d.

3. **Deploy the application**:
    ```sh
    kubectl apply -f kubernetes/deployment.yaml
    ```

4. **Access the website**:
    Open your browser and navigate to `https://localhost` (or the URL provided by your k3d setup).

## Contributing

Contributions are welcome! Please fork the repository and submit a pull request for any improvements or new features.

## Contact

For any questions or inquiries, please contact me at milesstorm@gmail.com.

---

Thank you for visiting my personal website repository. Enjoy exploring the technologies and features showcased here!

---

[![Website](https://img.shields.io/website-up-down-green-red/https/milesstorm.com.svg)](https://milesstorm.com)
