"use client"
import { useId } from "react"
import { useTranslations } from "next-intl"
import { useGithubRepositories } from "@/hooks/use-github-repositories"
import { Input } from "@/components/ui/input"
import { WorkspaceCatalogError } from "./workspace-catalog-error"

export type RepositorySelection = { fullName: string; branch: string }
/** Suggestions are not a complete inventory; typed values are verified on submit. */
export function WorkspaceRepositoryPicker({
  value,
  onChange,
  disabled,
}: {
  value: RepositorySelection
  onChange: (value: RepositorySelection) => void
  disabled: boolean
}) {
  const t = useTranslations("WorkspaceCatalog")
  const id = useId()
  const { repos, loading, error, retry } = useGithubRepositories(true)
  return (
    <fieldset
      disabled={disabled}
      className="space-y-3 rounded-md border border-border p-3"
    >
      <div className="space-y-1.5">
        <label className="text-xs font-medium" htmlFor={`${id}-repo`}>
          {t("repository")}
        </label>
        <Input
          id={`${id}-repo`}
          list={`${id}-repos`}
          placeholder="owner/repository"
          value={value.fullName}
          onChange={(event) =>
            onChange({ ...value, fullName: event.target.value })
          }
        />
        <datalist id={`${id}-repos`}>
          {repos.map((repo) => (
            <option key={repo.full_name} value={repo.full_name} />
          ))}
        </datalist>
      </div>
      <div className="space-y-1.5">
        <label className="text-xs font-medium" htmlFor={`${id}-branch`}>
          {t("startingBranch")}
        </label>
        <Input
          id={`${id}-branch`}
          value={value.branch}
          onChange={(event) =>
            onChange({ ...value, branch: event.target.value })
          }
        />
      </div>
      <p className="text-xs text-muted-foreground">{t("repositoryHint")}</p>
      {loading && (
        <p role="status" className="text-xs text-muted-foreground">
          {t("loadingRepositories")}
        </p>
      )}
      {error != null && <WorkspaceCatalogError error={error} retry={retry} />}
    </fieldset>
  )
}
