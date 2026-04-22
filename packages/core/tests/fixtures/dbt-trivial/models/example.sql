{{ config(materialized='table') }}

select 1 as id, 'hello' as name
union all
select 2 as id, 'world' as name
