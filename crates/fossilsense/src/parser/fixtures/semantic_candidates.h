#ifndef SEMANTIC_CANDIDATES_H
#define SEMANTIC_CANDIDATES_H

#define SEMANTIC_CANDIDATE_FLAG 1

typedef struct CandidateRecord {
    int id;
} CandidateRecord, *CandidateRecordPtr;

extern int guarded_object;
int guarded_api(CandidateRecordPtr record);

#ifdef ENABLE_OPTIONAL_CANDIDATE
int optional_guarded_api(void);
#endif

#endif
