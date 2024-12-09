
# PG Task Queue Experiment Project

## Overview
This project is an educational tool designed to demonstrate the implementation of a task queue using PostgreSQL. It aims to provide a clear understanding of how task queues work and how they can be efficiently managed using a relational database.

## Features
- Task creation and management

## TODO
- Task prioritization
- Task execution and monitoring
- Error handling and retries
- Scalability and performance optimization

## Prerequisites
- PostgreSQL 16 or higher
- Docker

## Installation
1. Clone the repository:
   ```
   git clone https://github.com/andycancado/pg-task-queue.git
   ```
2. Navigate to the project directory:
   ```
   cd pg-task-queue
   ```

## Usage
`` bash
docker compose --env-file .env up -d
cargo run

```

`` bash
docker compose exec postgres psql -U taskuser -d taskdb 
taskdb=# SELECT create_test_task('print_test', '{"data": "andy"}'::jsonb);

```
```

## Contributing
Contributions are welcome! Please fork the repository and submit a pull request with your changes.

## License
This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.

