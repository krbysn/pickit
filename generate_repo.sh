#!/bin/bash

REPO_ROOT="./samplerepo"
FILES_PER_DIR=10

echo "Creating repository at $REPO_ROOT"
mkdir -p "$REPO_ROOT"
cd "$REPO_ROOT" || exit

# Initialize git repo
git init

create_files() {
    local dir=$1
    for i in $(seq 1 $FILES_PER_DIR); do
        echo "file content for $dir/file$i.txt" > "$dir/file$i.txt"
    done
}

# Level 1: samplerepo (already created)

# Level 2: 15 subdirectories
for i in $(seq 1 15); do
    DIR2="dir2_$i"
    mkdir -p "$DIR2"
    create_files "$DIR2"

    # Level 3: 1000 subdirectories
    for j in $(seq 1 1000); do
        DIR3="$DIR2/dir3_$j"
        mkdir -p "$DIR3"
        create_files "$DIR3"

        # Level 4: 1 subdirectory
        DIR4="$DIR3/dir4_1"
        mkdir -p "$DIR4"
        create_files "$DIR4"

        # Level 5: 3 subdirectories
        for k in $(seq 1 3); do
            DIR5="$DIR4/dir5_$k"
            mkdir -p "$DIR5"
            create_files "$DIR5"

            # Level 6: 1 subdirectory
            DIR6="$DIR5/dir6_1"
            mkdir -p "$DIR6"
            create_files "$DIR6"
        done
    done
done

echo "Adding files to git..."
git add .
echo "Committing files..."
git commit -m "Initial commit of large sample repository"

echo "Repository creation complete at $REPO_ROOT"
