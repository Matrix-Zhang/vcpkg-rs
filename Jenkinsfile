pipeline {
    agent any

    stages {
        stage('Build') {
            steps {
                sh 'cargo build --all'
                sh 'build/build-linux'
            }
        }
        stage('Test') {
            steps {
                sh 'cargo test --all'
                sh 'cargo run --manifest-path vcpkg_cli/Cargo.toml -- probe sqlite3'
                sh 'cargo run --manifest-path systest/Cargo.toml'
              }
        }
    }

    post {
        failure {
            slackSend channel: '#jenkins',
                color: 'bad',
                message: "The pipeline ${currentBuild.fullDisplayName} failed. ${env.JENKINS_URL}/blue/organizations/jenkins/${env.JOB_NAME}/activity",
                failOnError: true
        }
        success {
            slackSend channel: '#jenkins',
                color: 'good',
                message: "The pipeline ${currentBuild.fullDisplayName} succeeded. ${env.JENKINS_URL}/blue/organizations/jenkins/${env.JOB_NAME}/activity",
                failOnError: true
        }
    }
}